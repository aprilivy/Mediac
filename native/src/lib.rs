use std::fmt;
use std::slice;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jni::objects::{JByteBuffer, JClass, JString};
use jni::{Env, bind_java_type};
use windows::Foundation::DateTime;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapBounds, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat,
    BitmapTransform, ColorManagementMode, ExifOrientationMode,
};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    GlobalSystemMediaTransportControlsSessionTimelineProperties as TimelineProperties,
};
use windows::Storage::Streams::IRandomAccessStreamReference;
use windows::core::Result;

bind_java_type! {
    NowPlaying => pet.aprl.mediac.client.MediacClient::NowPlaying,
    constructors {
        fn new(
            title: JString,
            artist: JString,
            playing: jboolean,
            time: JString,
            progress: jfloat,
            artwork_id: jlong,
        ),
    },
}

bind_java_type! {
    Mediac => pet.aprl.mediac.client.MediacClient,
    type_map = {
        NowPlaying => pet.aprl.mediac.client.MediacClient::NowPlaying,
    },
    native_methods {
        static extern fn now_playing() -> NowPlaying,
        static extern fn copy_artwork(rgba: java.nio.ByteBuffer),
        static extern fn toggle_play_pause(),
        static extern fn skip_next(),
        static extern fn skip_previous(),
    },
}

const ARTWORK_PIXELS: u32 = 128;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

static TRACK: Mutex<Option<Arc<Track>>> = Mutex::new(None);

static POLLER: LazyLock<Sender<Command>> = LazyLock::new(|| {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("Mediac".into())
        .spawn(move || poll(&receiver))
        .expect("failed to start the Mediac thread");
    sender
});

struct Track {
    metadata: Metadata,
    playing: bool,
    timeline: Option<Timeline>,
    artwork: Option<Arc<Artwork>>,
}

#[derive(PartialEq)]
struct Metadata {
    title: String,
    artist: String,
    album: String,
    has_thumbnail: bool,
}

struct Timeline {
    position: Duration,
    duration: Duration,
    updated_at: SystemTime,
    rate: f64,
}

struct Artwork {
    id: i64,
    rgba: Vec<u8>,
}

#[derive(Clone, Copy)]
enum Command {
    TogglePlayPause,
    SkipNext,
    SkipPrevious,
}

struct Clock(Duration);

impl MediacNativeInterface for MediacAPI {
    type Error = jni::errors::Error;

    fn now_playing<'local>(
        env: &mut Env<'local>,
        _class: JClass<'local>,
    ) -> jni::errors::Result<NowPlaying<'local>> {
        LazyLock::force(&POLLER);
        let Some(track) = current_track() else {
            return Ok(NowPlaying::null());
        };

        let (time, progress) = match &track.timeline {
            Some(timeline) => {
                let position = timeline.position_at(SystemTime::now());
                let time = format!("{} / {}", Clock(position), Clock(timeline.duration));
                (
                    JString::from_str(env, time)?,
                    position.div_duration_f32(timeline.duration),
                )
            }
            None => (JString::null(), 0.0),
        };
        let title = JString::from_str(env, &track.metadata.title)?;
        let artist = JString::from_str(env, &track.metadata.artist)?;
        let artwork_id = track.artwork.as_ref().map_or(0, |artwork| artwork.id);

        NowPlaying::new(
            env,
            &title,
            &artist,
            track.playing,
            &time,
            progress,
            artwork_id,
        )
    }

    fn copy_artwork<'local>(
        env: &mut Env<'local>,
        _class: JClass<'local>,
        rgba: JByteBuffer<'local>,
    ) -> jni::errors::Result<()> {
        if let Some(artwork) = current_track().and_then(|track| track.artwork.clone()) {
            let address = env.get_direct_buffer_address(&rgba)?;
            let capacity = env.get_direct_buffer_capacity(&rgba)?;
            unsafe { slice::from_raw_parts_mut(address, capacity) }.copy_from_slice(&artwork.rgba);
        }
        Ok(())
    }

    fn toggle_play_pause<'local>(
        _env: &mut Env<'local>,
        _class: JClass<'local>,
    ) -> jni::errors::Result<()> {
        Command::TogglePlayPause.send();
        Ok(())
    }

    fn skip_next<'local>(
        _env: &mut Env<'local>,
        _class: JClass<'local>,
    ) -> jni::errors::Result<()> {
        Command::SkipNext.send();
        Ok(())
    }

    fn skip_previous<'local>(
        _env: &mut Env<'local>,
        _class: JClass<'local>,
    ) -> jni::errors::Result<()> {
        Command::SkipPrevious.send();
        Ok(())
    }
}

fn current_track() -> Option<Arc<Track>> {
    TRACK.lock().unwrap().clone()
}

fn poll(commands: &Receiver<Command>) {
    let mut last_error = None;
    loop {
        let result = match commands.recv_timeout(POLL_INTERVAL) {
            Ok(command) => command.run(),
            Err(RecvTimeoutError::Timeout) => refresh(),
            Err(RecvTimeoutError::Disconnected) => return,
        };

        let error = result.err();
        if let Some(error) = &error
            && last_error.as_ref() != Some(error)
        {
            eprintln!("Mediac: {error}");
        }
        last_error = error;
    }
}

fn refresh() -> Result<()> {
    let previous = current_track();
    let current = Track::read(previous.as_deref());
    *TRACK.lock().unwrap() = current.clone().unwrap_or_default();
    current.map(|_| ())
}

fn current_session() -> Result<Option<Session>> {
    SessionManager::RequestAsync()?
        .join()?
        .GetCurrentSession()
        .optional()
}

impl Command {
    fn send(self) {
        let _ = POLLER.send(self);
    }

    fn run(self) -> Result<()> {
        let Some(session) = current_session()? else {
            return Ok(());
        };
        match self {
            Command::TogglePlayPause => session.TryTogglePlayPauseAsync()?,
            Command::SkipNext => session.TrySkipNextAsync()?,
            Command::SkipPrevious => session.TrySkipPreviousAsync()?,
        }
        .join()?;
        Ok(())
    }
}

impl Track {
    fn read(previous: Option<&Track>) -> Result<Option<Arc<Track>>> {
        let Some(session) = current_session()? else {
            return Ok(None);
        };
        let properties = session.TryGetMediaPropertiesAsync()?.join()?;
        let thumbnail = properties.Thumbnail().optional()?;
        let metadata = Metadata {
            title: properties.Title()?.to_string_lossy(),
            artist: properties.Artist()?.to_string_lossy(),
            album: properties.AlbumTitle()?.to_string_lossy(),
            has_thumbnail: thumbnail.is_some(),
        };

        let artwork = match (previous, thumbnail) {
            (Some(previous), _) if previous.metadata == metadata => previous.artwork.clone(),
            (_, Some(thumbnail)) => Artwork::decode(&thumbnail)
                .inspect_err(|error| {
                    eprintln!(
                        "Mediac: failed to decode the artwork for {}: {error}",
                        metadata.title
                    )
                })
                .ok()
                .map(Arc::new),
            (_, None) => None,
        };

        let playback = session.GetPlaybackInfo()?;
        let playing = playback.PlaybackStatus()? == PlaybackStatus::Playing;
        let rate = match playback.PlaybackRate().optional()? {
            _ if !playing => 0.0,
            Some(rate) => rate.Value()?.max(0.0),
            None => 1.0,
        };

        Ok(Some(Arc::new(Track {
            metadata,
            playing,
            timeline: Timeline::read(&session.GetTimelineProperties()?, rate)?,
            artwork,
        })))
    }
}

impl Timeline {
    fn read(properties: &TimelineProperties, rate: f64) -> Result<Option<Timeline>> {
        let start = Duration::from(properties.StartTime()?);
        let duration = Duration::from(properties.EndTime()?).saturating_sub(start);
        if duration.is_zero() {
            return Ok(None);
        }
        Ok(Some(Timeline {
            position: Duration::from(properties.Position()?).saturating_sub(start),
            duration,
            updated_at: system_time(properties.LastUpdatedTime()?),
            rate,
        }))
    }

    fn position_at(&self, now: SystemTime) -> Duration {
        let elapsed = now.duration_since(self.updated_at).unwrap_or_default();
        (self.position + elapsed.mul_f64(self.rate)).min(self.duration)
    }
}

impl Artwork {
    fn decode(thumbnail: &IRandomAccessStreamReference) -> Result<Artwork> {
        static NEXT_ID: AtomicI64 = AtomicI64::new(1);

        let decoder = BitmapDecoder::CreateAsync(&thumbnail.OpenReadAsync()?.join()?)?.join()?;
        let (width, height) = (decoder.PixelWidth()?, decoder.PixelHeight()?);
        let scale = f64::from(ARTWORK_PIXELS) / f64::from(width.min(height));
        let scaled = |length: u32| (f64::from(length) * scale).round() as u32;

        let transform = BitmapTransform::new()?;
        transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
        transform.SetScaledWidth(scaled(width))?;
        transform.SetScaledHeight(scaled(height))?;
        transform.SetBounds(BitmapBounds {
            X: (scaled(decoder.OrientedPixelWidth()?) - ARTWORK_PIXELS) / 2,
            Y: (scaled(decoder.OrientedPixelHeight()?) - ARTWORK_PIXELS) / 2,
            Width: ARTWORK_PIXELS,
            Height: ARTWORK_PIXELS,
        })?;

        let pixels = decoder
            .GetPixelDataTransformedAsync(
                BitmapPixelFormat::Rgba8,
                BitmapAlphaMode::Straight,
                &transform,
                ExifOrientationMode::RespectExifOrientation,
                ColorManagementMode::ColorManageToSRgb,
            )?
            .join()?
            .DetachPixelData()?;

        Ok(Artwork {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            rgba: pixels.to_vec(),
        })
    }
}

impl fmt::Display for Clock {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let seconds = self.0.as_secs();
        match seconds / 3600 {
            0 => write!(f, "{}:{:02}", seconds / 60, seconds % 60),
            hours => write!(f, "{hours}:{:02}:{:02}", seconds / 60 % 60, seconds % 60),
        }
    }
}

fn system_time(date: DateTime) -> SystemTime {
    let ticks = u64::try_from(date.UniversalTime - UNIX_EPOCH_TICKS).unwrap_or_default();
    UNIX_EPOCH + Duration::from_nanos(ticks * 100)
}

trait Optional<T> {
    fn optional(self) -> Result<Option<T>>;
}

impl<T> Optional<T> for Result<T> {
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.code().is_ok() => Ok(None),
            Err(error) => Err(error),
        }
    }
}
