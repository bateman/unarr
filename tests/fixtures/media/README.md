# Media clips

Synthetic video files the mock download clients put in a completed
download's folder (`POST /mock/<client>/complete`, `clip=`, `extras=`), so
that the real Sonarr, while the goldens are recorded, and Unarr, in the
tests, import something ffprobe can read. Nothing in them comes from a real
release: a test pattern, silence, one line of subtitle.

| File | Content | Why |
|------|---------|-----|
| `episode.mkv` | 64x64 h264 at 2 fps, 10 min 10 s, one silent AAC mono track (`eng`), one SRT subtitle (`eng`) | Longer than the ten minutes the originals' sample detection asks of a 60-minute series, so it imports for any series in the fixture. |
| `episode-1080.mkv` | The same at 1920x1080, 1 fps | A real quality upgrade: the originals take the resolution from media info, so a 64x64 clip imports as 480p whatever its name says. |
| `episode-dual.mkv` | The same with two audio tracks (`eng`, `ita`) and two subtitles (`eng`, `ita`) | Audio languages from media info, the `{MediaInfo AudioLanguages}` naming token. |
| `sample.mkv` | 45 s, one silent track | Rejected as a sample. |

Generated with ffmpeg 6.1 (the exact bytes do not matter, the goldens were
recorded from the committed files; regenerate only together with them):

```sh
printf '1\n00:00:01,000 --> 00:00:05,000\nFixture subtitle\n' > sub.srt
ffmpeg -f lavfi -i testsrc=size=64x64:rate=2:duration=610 -f lavfi -i anullsrc=r=8000:cl=mono -i sub.srt \
  -map 0:v -map 1:a -map 2:s -t 610 -c:v libx264 -preset veryfast -crf 51 -pix_fmt yuv420p \
  -c:a aac -b:a 8k -c:s srt -metadata:s:a:0 language=eng -metadata:s:s:0 language=eng episode.mkv
ffmpeg -f lavfi -i testsrc=size=1920x1080:rate=1:duration=610 -f lavfi -i anullsrc=r=8000:cl=mono -i sub.srt \
  -map 0:v -map 1:a -map 2:s -t 610 -c:v libx264 -preset veryfast -crf 51 -pix_fmt yuv420p \
  -c:a aac -b:a 8k -c:s srt -metadata:s:a:0 language=eng -metadata:s:s:0 language=eng episode-1080.mkv
ffmpeg -f lavfi -i testsrc=size=64x64:rate=2:duration=610 -f lavfi -i anullsrc=r=8000:cl=mono \
  -f lavfi -i anullsrc=r=8000:cl=stereo -i sub.srt -i sub.srt \
  -map 0:v -map 1:a -map 2:a -map 3:s -map 4:s -t 610 -c:v libx264 -preset veryfast -crf 51 -pix_fmt yuv420p \
  -c:a aac -b:a 8k -c:s srt -metadata:s:a:0 language=eng -metadata:s:a:1 language=ita \
  -metadata:s:s:0 language=eng -metadata:s:s:1 language=ita episode-dual.mkv
ffmpeg -f lavfi -i testsrc=size=64x64:rate=2:duration=45 -f lavfi -i anullsrc=r=8000:cl=mono -t 45 \
  -c:v libx264 -preset veryfast -crf 51 -pix_fmt yuv420p -c:a aac -b:a 8k -metadata:s:a:0 language=eng sample.mkv
```

Sonarr bundles its own ffprobe (5.1 in the pinned release), and its output
differs from a distribution's 6.1 on these files in one field that matters:
the video stream's `codec_tag_string` is `x264` for 5.1 and `[0][0][0][0]`
for 6.1, which the originals' formatter turns into `x264` against `h264`.
The tests use the bundled binary when `.cache/sonarr/Sonarr/ffprobe` exists
(`make ui` leaves it there) and the one on the `PATH` otherwise.
