//! Final output boundary tests use private roots and do not contact providers or change auth.
use serde_json::Value;
use std::{path::Path, process::Command};

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_relay"))
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(args)
        .env("TERM", "xterm-256color")
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap()
}

#[test]
fn pipes_no_color_json_and_hidden_protocols_are_plain() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["profiles"],
        vec!["status"],
        vec!["--help"],
        vec!["profile", "list"],
        vec!["profile", "remove", "missing"],
        vec![
            "switch-request",
            "main",
            "--session",
            "11111111-1111-4111-8111-111111111111",
            "--project",
            root.path().to_str().unwrap(),
        ],
    ] {
        let result = run(root.path(), &args);
        assert!(
            !result.stdout.contains(&27) && !result.stderr.contains(&27),
            "{args:?}"
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_relay"))
        .args(["profiles", "--config-root"])
        .arg(root.path().join("config"))
        .env("NO_COLOR", "")
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap();
    assert!(!output.stdout.contains(&27) && !output.stderr.contains(&27));
    for args in [
        vec!["--json", "profiles"],
        vec!["--json", "status"],
        vec!["--json", "profile", "remove", "missing"],
    ] {
        let result = run(root.path(), &args);
        let bytes = if result.status.success() {
            &result.stdout
        } else {
            &result.stderr
        };
        assert!(!bytes.contains(&27));
        let json: Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["ok"], result.status.success());
        if args[1] == "profiles" {
            assert_eq!(json["data"]["profiles"], serde_json::json!([]));
        }
    }
}

#[test]
fn scratch_success_and_error_keep_concise_actions_and_codes() {
    let root = tempfile::tempdir().unwrap();
    let added = run(root.path(), &["profile", "add", "demo"]);
    assert!(added.status.success(), "{added:?}");
    let removed = run(root.path(), &["profile", "remove", "demo"]);
    let text = String::from_utf8(removed.stdout).unwrap();
    assert!(
        removed.status.success()
            && text.contains("Removed profile: demo")
            && text.contains("retained")
    );
    let missing = run(root.path(), &["profile", "remove", "missing"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .starts_with("error [profile_not_found]:")
    );
}

#[cfg(unix)]
#[test]
fn real_terminal_obeys_color_no_color_dumb_and_json_policy() {
    let root = tempfile::tempdir().unwrap();
    let script = r#"
import os,pty,select,subprocess,sys,time,json
exe,root=sys.argv[1:]
for mode in ('tty','no-color','dumb','json','control'):
    master,slave=pty.openpty()
    env=os.environ.copy()
    env.pop('NO_COLOR',None)
    env.update(TERM='xterm-256color', COLUMNS='60')
    if mode=='no-color': env['NO_COLOR']=''
    if mode=='dumb': env['TERM']='dumb'
    args=[exe,'--config-root',root+'/config','--state-root',root+'/state']
    if mode=='json': args+=['--json']
    if mode=='control': args+=['switch-request','main','--session','11111111-1111-4111-8111-111111111111','--project',root]
    else: args+=['profiles']
    process=subprocess.Popen(args,stdin=slave,stdout=slave,stderr=slave,env=env)
    os.close(slave)
    data=b''; deadline=time.monotonic()+10
    while time.monotonic()<deadline:
        ready,_,_=select.select([master],[],[],.1)
        if ready:
            try: chunk=os.read(master,65536)
            except OSError: break
            if not chunk: break
            data+=chunk
        elif process.poll() is not None: break
    if process.poll() is None: process.kill(); raise AssertionError('terminal command timed out')
    assert process.wait()==0,(mode,data)
    os.close(master)
    assert (b'\x1b[' in data)==(mode=='tty'),(mode,data)
    if mode=='json': assert json.loads(data)['schema_version']==1
print('TTY color; NO_COLOR, TERM=dumb, JSON, control paths plain: PASS')
"#;
    let result = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_relay"))
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
