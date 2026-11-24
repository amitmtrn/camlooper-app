# Phone-stream fixtures

Played by the test-only phone simulator (`tests/support/phone_sim.rs`) in
`tests/phone_stream_loopback.rs`, and used by `remote::stream::media` unit tests (SPS parsing).
3 s of `testsrc2` at 640x360, 30 fps, a keyframe every second. Regenerate with:

```bash
ffmpeg -f lavfi -i testsrc2=size=640x360:rate=30 -t 3 -c:v libx264 -profile:v baseline \
  -pix_fmt yuv420p -g 30 -bf 0 -b:v 250k -maxrate 300k -bufsize 300k -x264-params slices=1 \
  h264-640x360.h264
ffmpeg -f lavfi -i testsrc2=size=640x360:rate=30 -t 3 -c:v libvpx -g 30 -b:v 150k \
  -deadline realtime -cpu-used 8 vp8-640x360.ivf
```

One slice per frame matters: the simulator splits the H.264 file into access units by VCL NAL.
