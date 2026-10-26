# Video test vectors

Copied unchanged from web-platform-tests (BSD 3-Clause, https://github.com/web-platform-tests/wpt), for the video
decoding and Media Source tests (`plugins/korvus-render/src/video_decode.rs`, `core/korvus-webapi/src/core/media/`,
`core/korvus-runtime/tests/video_decode_live.rs`). `../v.mp4` is WPT's `media/2x2-green.mp4`.

| File | WPT path | What it is |
|---|---|---|
| `four-colors.mp4` | `webcodecs/four-colors.mp4` | AV1, one frame: yellow, red, blue and green quadrants (identity matrix) |
| `av1.mp4` | `webcodecs/av1.mp4` | AV1, 320x240 |
| `h264.mp4` | `webcodecs/h264.mp4` | H.264 High, 320x240, ten frames at 10 fps |
| `test-v-128k-320x240-24fps-8kfr.mp4` | `media-source/mp4/` | fragmented MP4, H.264, 24 fps, a keyframe every 8 frames |
| `test-av-384k-44100Hz-1ch-320x240-30fps-10kfr.webm` | `media-source/webm/` | WebM, VP8 video and Vorbis audio |
