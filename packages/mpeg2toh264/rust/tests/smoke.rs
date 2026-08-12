//! End-to-end smoke test over a real MPEG-2 elementary stream.
//!
//! Points at an `.m2v` via `CHIBITV_MPEG2TOH264_TEST_ES` -- for example one of
//! mpeg2toh264's own `testdata/` files -- and is skipped when unset, so a
//! checkout without fixtures stays green. Set `CHIBITV_MPEG2TOH264_TEST_OUT`
//! to also write the concatenated fragments as an `.mp4` for inspection with
//! ffprobe or a browser.

use chibitv_mpeg2toh264::Transcoder;

#[test]
fn converts_an_elementary_stream_to_fragments() {
    let Ok(path) = std::env::var("CHIBITV_MPEG2TOH264_TEST_ES") else {
        eprintln!("CHIBITV_MPEG2TOH264_TEST_ES is unset; skipping");
        return;
    };
    let data = std::fs::read(&path).expect("the fixture should be readable");

    let mut transcoder = Transcoder::new(2.0, 4);
    let mut fragments = transcoder.push_video(&data, 0.0).expect("conversion");
    fragments.extend(transcoder.finish().expect("flush"));

    assert!(!fragments.is_empty(), "the stream produced no fragments");
    assert_eq!(transcoder.units_skipped(), 0.0);

    let first = &fragments[0];
    let mime = first
        .mime_codec()
        .expect("the first fragment declares a MIME type");
    assert!(
        mime.starts_with("video/mp4; codecs=\"avc1."),
        "unexpected MIME type: {mime}"
    );
    assert!(first.init_segment().is_some_and(|init| !init.is_empty()));

    let mut starts = Vec::new();
    for fragment in &fragments {
        assert!(!fragment.media_segment().is_empty());
        starts.push(fragment.start_seconds());
    }
    assert!(
        starts.windows(2).all(|pair| pair[0] < pair[1]),
        "fragment start times are not monotonic: {starts:?}"
    );

    if let Ok(out) = std::env::var("CHIBITV_MPEG2TOH264_TEST_OUT") {
        let mut file = Vec::new();
        for fragment in &fragments {
            if let Some(init) = fragment.init_segment() {
                file.extend_from_slice(&init);
            }
            file.extend_from_slice(&fragment.media_segment());
        }
        std::fs::write(out, file).expect("the output path should be writable");
    }
}
