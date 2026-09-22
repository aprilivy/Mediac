package pet.aprl.mediac.client;

import java.io.FileNotFoundException;
import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.zip.CRC32;

import com.mojang.blaze3d.platform.InputConstants;
import com.mojang.blaze3d.platform.NativeImage;
import com.mojang.blaze3d.systems.RenderSystem;
import com.mojang.blaze3d.textures.FilterMode;
import net.fabricmc.api.ClientModInitializer;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientTickEvents;
import net.fabricmc.fabric.api.client.keymapping.v1.KeyMappingHelper;
import net.fabricmc.fabric.api.client.rendering.v1.hud.HudElementRegistry;
import net.fabricmc.fabric.api.client.rendering.v1.hud.VanillaHudElements;
import net.minecraft.client.DeltaTracker;
import net.minecraft.client.KeyMapping;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.ActiveTextCollector;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.renderer.RenderPipelines;
import net.minecraft.client.renderer.texture.DynamicTexture;
import net.minecraft.client.renderer.texture.TextureManager;
import net.minecraft.network.chat.Component;
import net.minecraft.resources.Identifier;
import net.minecraft.util.CommonColors;
import org.jspecify.annotations.Nullable;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public class MediacClient implements ClientModInitializer {

    private static final Logger LOGGER = LoggerFactory.getLogger(MediacClient.class);
    private static final String MOD_ID = "mediac";
    private static final String LIBRARY = "/natives/mediac.dll";
    private static final Identifier ARTWORK = Identifier.fromNamespaceAndPath(MOD_ID, "artwork");
    private static final Component PAUSED = Component.translatable("mediac.hud.paused").withColor(CommonColors.LIGHT_GRAY);

    private static final int ARTWORK_PIXELS = 128;
    private static final int MARGIN = 4;
    private static final int PADDING = 4;
    private static final int ARTWORK_SIZE = 32;
    private static final int TEXT_WIDTH = 120;
    private static final int LINE_HEIGHT = 10;
    private static final int PROGRESS_HEIGHT = 2;
    private static final int BACKGROUND = 0x80000000;
    private static final int PROGRESS_TRACK = 0x40FFFFFF;

    private boolean hidden;
    private long artworkId;

    private record NowPlaying(String title, String artist, boolean playing, @Nullable String time, float progress, long artworkId) {
    }

    private static native @Nullable NowPlaying nowPlaying();

    private static native void copyArtwork(ByteBuffer rgba);

    private static native void togglePlayPause();

    private static native void skipNext();

    private static native void skipPrevious();

    @Override
    public void onInitializeClient() {
        if (!System.getProperty("os.name").startsWith("Windows") || !System.getProperty("os.arch").equals("amd64")) {
            LOGGER.warn("Media controls are disabled because they need x86-64 Windows");
            return;
        }
        try {
            loadLibrary();
        } catch (IOException | UnsatisfiedLinkError e) {
            LOGGER.error("Media controls are disabled because the native library didn't load", e);
            return;
        }

        KeyMapping.Category category = KeyMapping.Category.register(Identifier.fromNamespaceAndPath(MOD_ID, "controls"));
        KeyMapping playPause = registerKey("play_pause", category);
        KeyMapping next = registerKey("next", category);
        KeyMapping previous = registerKey("previous", category);
        KeyMapping toggleHud = registerKey("toggle_hud", category);
        ClientTickEvents.END_CLIENT_TICK.register(client -> {
            while (playPause.consumeClick()) {
                togglePlayPause();
            }
            while (next.consumeClick()) {
                skipNext();
            }
            while (previous.consumeClick()) {
                skipPrevious();
            }
            while (toggleHud.consumeClick()) {
                hidden = !hidden;
            }
        });

        HudElementRegistry.attachElementAfter(VanillaHudElements.BOSS_BAR, Identifier.fromNamespaceAndPath(MOD_ID, "now_playing"), this::extractHud);
    }

    private static void loadLibrary() throws IOException {
        byte[] library;
        try (InputStream in = MediacClient.class.getResourceAsStream(LIBRARY)) {
            if (in == null) {
                throw new FileNotFoundException(LIBRARY + " is missing from the mod jar");
            }
            library = in.readAllBytes();
        }

        CRC32 crc = new CRC32();
        crc.update(library);
        Path path = Path.of(System.getProperty("java.io.tmpdir"), "mediac", "mediac-%08x.dll".formatted(crc.getValue()));
        if (Files.notExists(path)) {
            Files.createDirectories(path.getParent());
            Path temp = Files.createTempFile(path.getParent(), "mediac-", ".tmp");
            try {
                Files.write(temp, library);
                Files.move(temp, path, StandardCopyOption.ATOMIC_MOVE);
            } catch (IOException e) {
                if (Files.notExists(path)) {
                    throw e;
                }
            } finally {
                Files.deleteIfExists(temp);
            }
        }
        System.load(path.toString());
    }

    private static KeyMapping registerKey(String name, KeyMapping.Category category) {
        return KeyMappingHelper.registerKeyMapping(
                new KeyMapping("key.mediac." + name, InputConstants.UNKNOWN.getValue(), category));
    }

    private void extractHud(GuiGraphicsExtractor graphics, DeltaTracker deltaTracker) {
        Minecraft minecraft = Minecraft.getInstance();
        if (hidden || minecraft.debugEntries.isOverlayVisible()) {
            return;
        }
        NowPlaying nowPlaying = nowPlaying();
        if (nowPlaying == null) {
            return;
        }

        int top = MARGIN + PADDING;
        int artworkX = MARGIN + PADDING;
        int textX = artworkX + ARTWORK_SIZE + PADDING;
        int textRight = textX + TEXT_WIDTH;
        graphics.fill(MARGIN, MARGIN, textRight + PADDING, top + ARTWORK_SIZE + PADDING, BACKGROUND);

        if (nowPlaying.artworkId() != artworkId) {
            artworkId = nowPlaying.artworkId();
            TextureManager textures = minecraft.getTextureManager();
            if (artworkId == 0) {
                textures.release(ARTWORK);
            } else {
                NativeImage image = new NativeImage(ARTWORK_PIXELS, ARTWORK_PIXELS, false);
                copyArtwork(image.getPixelBytes());
                textures.register(ARTWORK, new DynamicTexture(() -> "Mediac artwork", image) {
                    {
                        sampler = RenderSystem.getSamplerCache().getClampToEdge(FilterMode.LINEAR);
                    }
                });
            }
        }
        if (artworkId != 0) {
            graphics.blit(RenderPipelines.GUI_TEXTURED, ARTWORK, artworkX, top, 0, 0,
                    ARTWORK_SIZE, ARTWORK_SIZE, ARTWORK_SIZE, ARTWORK_SIZE);
        } else {
            graphics.fill(artworkX, top, artworkX + ARTWORK_SIZE, top + ARTWORK_SIZE, CommonColors.DARK_GRAY);
        }

        ActiveTextCollector text = graphics.textRenderer();
        text.acceptScrolling(Component.literal(nowPlaying.title()),
                textX, textX, textRight, top, top + LINE_HEIGHT);
        text.acceptScrolling(Component.literal(nowPlaying.artist()).withColor(CommonColors.LIGHT_GRAY),
                textX, textX, textRight, top + LINE_HEIGHT, top + 2 * LINE_HEIGHT);
        if (!nowPlaying.playing()) {
            text.acceptScrolling(PAUSED, textRight, textX, textRight, top + 2 * LINE_HEIGHT, top + 3 * LINE_HEIGHT);
        }
        if (nowPlaying.time() != null) {
            text.acceptScrolling(Component.literal(nowPlaying.time()).withColor(CommonColors.LIGHT_GRAY),
                    textX, textX, textRight, top + 2 * LINE_HEIGHT, top + 3 * LINE_HEIGHT);

            int progressY = top + ARTWORK_SIZE - PROGRESS_HEIGHT;
            graphics.fill(textX, progressY, textRight, progressY + PROGRESS_HEIGHT, PROGRESS_TRACK);
            graphics.fill(textX, progressY, textX + (int) (TEXT_WIDTH * nowPlaying.progress()), progressY + PROGRESS_HEIGHT, CommonColors.WHITE);
        }
    }
}
