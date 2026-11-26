//! The model server is sandboxed but untrusted: its stream and its log
//! lines must not crash the core.

use gardenwall_inference::client::SseParser;
use gardenwall_inference::launch::parse_listening_port;
use gardenwall_inference::placement::Placement;

#[test]
fn stream_parser_survives_mutation() {
    let seeds = vec![
        b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n: keep-alive\r\ndata: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]}}]}\n\ndata: [DONE]\n\n".to_vec(),
    ];
    gardenwall_fuzzkit::run("sse", &seeds, 5000, |bytes| {
        let mut p = SseParser::default();
        let mut out = Vec::new();
        // Fed in uneven pieces, as the network delivers it.
        for piece in bytes.chunks(7) {
            if p.push(piece, &mut out).is_err() {
                break;
            }
        }
    });
}

#[test]
fn log_parsers_survive_mutation() {
    gardenwall_fuzzkit::run_text(
        "server-log",
        &[
            "load_tensors:        CUDA0 model buffer size = 19634.06 MiB",
            "load_tensors: offloaded 65/65 layers to GPU",
            "llama_params_fit_impl: 21 overflowing layers",
            "main: server is listening on http://127.0.0.1:8080 - starting the main loop",
        ],
        5000,
        |line| {
            let mut p = Placement::default();
            p.observe(line);
            let _ = parse_listening_port(line);
        },
    );
}
