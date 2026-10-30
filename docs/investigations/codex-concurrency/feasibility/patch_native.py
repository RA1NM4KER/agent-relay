#!/usr/bin/env python3
"""Apply a research-only insertion to a disposable, exact upstream source checkout.

This is not a patch to installed Codex or Relay. Source applicability is tested
here; compiling/running the complete patched native TUI requires the lab procedure.
"""
import argparse
import difflib
import hashlib
import json
from pathlib import Path


EXPECTED = {
    "codex-rs/app-server-client/src/lib.rs": "e7df794ea809c492ae02e32fe2d34dcb63f39132",
    "codex-rs/tui/src/lib.rs": "1b0b08cb3a87b088fa21b61ef8fbb3626b93f114",
    "codex-rs/tui/src/app/thread_routing.rs": "e4ecae0ec92756ab19ad65dcb11779acb64794d6",
    "codex-rs/tui/src/app/reconnect.rs": "91dde41810f519d67c706762cd94dac0e4ed97e4",
}


def blob(data):
    return hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()


def once(text, old, new):
    if text.count(old) != 1:
        raise ValueError("upstream insertion anchor missing or ambiguous")
    return text.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("checkout", type=Path)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--diff", type=Path)
    args = parser.parse_args()
    checkout = args.checkout.resolve(strict=True)
    relay = Path(__file__).resolve().parents[4]
    if checkout == relay or (checkout / "crates/relay-cli").exists():
        raise ValueError("refusing to patch the Relay workspace")
    originals = {}
    for path, expected in EXPECTED.items():
        data = (checkout / path).read_bytes()
        if blob(data) != expected:
            raise ValueError(f"not the pinned original source: {path}")
        originals[path] = data.decode()
    updated = dict(originals)
    path = "codex-rs/app-server-client/src/lib.rs"
    text = once(updated[path], "mod cloud;", "pub mod relay_research;\nmod cloud;")
    for kind in ["handle", "client"]:
        old = '''    pub async fn request(&self, request: ClientRequest) -> IoResult<RequestResult> {
        match self {
            Self::InProcess(KIND) => KIND.request(request).await,
            Self::Remote(KIND) => KIND.request(request).await,
            Self::Cloud(KIND) => KIND.request(request).await,
        }
    }'''.replace("KIND", kind)
        new = '''    pub async fn request(&self, request: ClientRequest) -> IoResult<RequestResult> {
        let method = request.method_name();
        let mut ticket = if relay_research::requires_gate(method)? {
            let identity = serde_json::to_value(&request).map_err(IoError::other)?;
            Some(relay_research::Ticket::begin(method,
                identity.get("params").and_then(|p| p.get("threadId")).and_then(|v| v.as_str()))?)
        } else { None };
        // Forward the original typed request unchanged. No permission translation.
        let response = match self {
            Self::InProcess(KIND) => KIND.request(request).await,
            Self::Remote(KIND) => KIND.request(request).await,
            Self::Cloud(KIND) => KIND.request(request).await,
        };
        if let Some(ticket) = ticket.as_mut() {
            let value = response.as_ref().ok().and_then(|r| r.as_ref().ok());
            let thread = value.and_then(|v| v.get("thread")).and_then(|v| v.get("id")).and_then(|v| v.as_str());
            ticket.complete(if value.is_some() { "ok" } else { "unknown" }, thread)?;
        }
        response
    }'''.replace("KIND", kind)
        text = once(text, old, new)
    updated[path] = text
    path = "codex-rs/tui/src/lib.rs"
    updated[path] = once(updated[path], ") -> std::io::Result<AppExitInfo> {\n    system_motion::initialize().await;",
        ") -> std::io::Result<AppExitInfo> {\n    codex_app_server_client::relay_research::enable_from_env()?;\n    system_motion::initialize().await;")
    for path, variable in [("codex-rs/tui/src/app/thread_routing.rs", "thread_id"),
                           ("codex-rs/tui/src/app/reconnect.rs", "id")]:
        anchor = f"self.active_thread_id = Some({variable});"
        report = f'''{anchor}
        if let Err(error) = codex_app_server_client::relay_research::selection(
            self.primary_thread_id.map(|id| id.to_string()).as_deref(), &{variable}.to_string()) {{
            tracing::error!(%error, "research supervision selection failed; mutations will refuse");
        }}'''
        updated[path] = once(updated[path], anchor, report)
    added_path = "codex-rs/app-server-client/src/relay_research.rs"
    updated[added_path] = (Path(__file__).parent / "native_bridge.rs").read_text()
    diff = "".join("".join(difflib.unified_diff(originals.get(p, "").splitlines(True), text.splitlines(True),
                    fromfile="a/" + p, tofile="b/" + p)) for p, text in updated.items())
    if args.diff:
        args.diff.write_text(diff)
    if args.apply:
        for path, text in updated.items():
            (checkout / path).write_text(text)
    print(json.dumps({"source_applicable": True, "full_native_build_tested": False,
        "applied_to_disposable_checkout": args.apply, "files": list(updated)}, indent=2))


if __name__ == "__main__":
    main()
