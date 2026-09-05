//! Headless Unix PTYs exercise the real client, kernel resize and disconnect.
//! TERM cases are byte/protocol evidence, not GUI-emulator certification.
use crate::common::*;
use crate::vt100;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::Write;
use std::sync::{Arc, Mutex};

struct Terminal {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    output: Capture,
}

impl Terminal {
    fn attach(env: &TestEnv, session: &str, mode: &str, cols: u16, rows: u16, term: &str) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(ezpn_binary());
        command.env_clear();
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        for (key, value) in env.variables() {
            command.env(key, value);
        }
        command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        command.env("SHELL", "/bin/sh");
        command.env("TERM", term);
        command.env("LC_ALL", "C");
        command.cwd(env.root());
        command.args(["attach", session, mode]);
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let output = Arc::new(Mutex::new(Vec::new()));
        spawn_capture(pair.master.try_clone_reader().unwrap(), output.clone());
        let writer = pair.master.take_writer().unwrap();
        let terminal = Self {
            master: pair.master,
            writer,
            child,
            output,
        };
        wait_for_output(&terminal.output, "\x1b[?1049h", DEFAULT_TIMEOUT)
            .expect("client did not enter alternate screen");
        terminal
    }
    fn write(&mut self, text: &str) {
        self.writer.write_all(text.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }
    fn prefix(&mut self, key: char) {
        self.write(&format!("\x02{key}"));
    }
    fn resize(&self, cols: u16, rows: u16) {
        self.master
            .resize(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
    }
    fn wait_exit(&mut self) {
        let status = wait_for("terminal client exit", DEFAULT_TIMEOUT, || {
            self.child.try_wait().unwrap()
        })
        .unwrap_or_else(|error| panic!("{error}: {}", captured(&self.output)));
        assert!(
            status.success(),
            "client failed: {status:?}: {}",
            captured(&self.output)
        );
        wait_for_output(&self.output, "\x1b[?1049l", DEFAULT_TIMEOUT)
            .expect("alternate screen not restored");
        // The slave termios remain inspectable through the master after exit.
        let mut attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(self.master.as_raw_fd().unwrap(), attrs.as_mut_ptr()) },
            0
        );
        let attrs = unsafe { attrs.assume_init() };
        assert_ne!(attrs.c_lflag & libc::ICANON, 0, "client left terminal raw");
        assert_ne!(attrs.c_lflag & libc::ECHO, 0, "client left echo disabled");
    }
    fn probe(&mut self, env: &TestEnv, name: &str) -> String {
        self.write(&format!(
            "printf '%s:%s\\n' \"$$\" \"$RETAINED\" > {}/{name}\r",
            env.root().display()
        ));
        file_text(env, name, &self.output)
    }
    fn size(&mut self, env: &TestEnv, name: &str) -> (u16, u16) {
        self.write(&format!("stty size > {}/{name}\r", env.root().display()));
        let text = file_text(env, name, &self.output);
        let values: Vec<u16> = text
            .split_whitespace()
            .map(|v| v.parse().unwrap())
            .collect();
        assert_eq!(values.len(), 2);
        (values[0], values[1])
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn file_text(env: &TestEnv, name: &str, output: &Capture) -> String {
    wait_for(&format!("completed file {name}"), DEFAULT_TIMEOUT, || {
        std::fs::read_to_string(env.root().join(name))
            .ok()
            .filter(|s| s.ends_with('\n'))
    })
    .unwrap_or_else(|error| panic!("{error}: {}", captured(output)))
}

#[test]
fn pty_detach_reattach_preserves_shell_and_restores_terminal() {
    for term in [
        "xterm-256color",
        "screen-256color",
        "tmux-256color",
        "vt100",
    ] {
        let env = TestEnv::new();
        let mut daemon = spawn_daemon(&env, "pty");
        let mut first = Terminal::attach(&env, "pty", "--shared", 100, 32, term);
        first.write("RETAINED=live-state; printf 'visible-%s\\n' ready\r");
        wait_for_output(&first.output, "visible-ready", DEFAULT_TIMEOUT).unwrap();
        let before = first.probe(&env, "before");
        assert!(before.ends_with(":live-state\n"));
        first.prefix('d');
        first.wait_exit();
        daemon.assert_alive();
        let mut second = Terminal::attach(&env, "pty", "--shared", 100, 32, term);
        assert_eq!(
            second.probe(&env, "after"),
            before,
            "{term}: shell state lost"
        );
        second.prefix('d');
        second.wait_exit();
    }
}

#[test]
fn pty_shared_readonly_resize_and_prefix_detach_are_client_local() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "resize");
    let mut first = Terminal::attach(&env, "resize", "--shared", 120, 40, "xterm-256color");
    let original = first.size(&env, "original-size");
    let mut second = Terminal::attach(&env, "resize", "--shared", 100, 32, "xterm-256color");
    let shared = first.size(&env, "shared-size");
    let mut readonly = Terminal::attach(&env, "resize", "--readonly", 80, 24, "xterm-256color");
    let small = first.size(&env, "small-size");
    assert_eq!(small, shared, "readonly attach resized writable clients");
    assert!(
        small.0 < original.0 && small.1 < original.1,
        "{small:?} vs {original:?}"
    );
    readonly.write(&format!("touch {}/forbidden\r", env.root().display()));
    readonly.resize(70, 20);
    wait_for(
        "readonly resize received by daemon",
        DEFAULT_TIMEOUT,
        || {
            let tree = ipc(&daemon, serde_json::json!({"cmd":"ls_tree"}));
            tree["ls_tree"]["sessions"][0]["clients"]
                .as_array()
                .unwrap()
                .iter()
                .any(|client| {
                    client["mode"] == "readonly" && client["size"] == serde_json::json!([70, 20])
                })
                .then_some(())
        },
    )
    .unwrap();
    for attempt in 0..10 {
        assert_eq!(
            first.size(&env, &format!("resized-{attempt}")),
            shared,
            "readonly resize changed writer geometry"
        );
    }
    assert!(
        !env.root().join("forbidden").exists(),
        "readonly input reached shell"
    );
    first.prefix('d');
    first.wait_exit();
    assert!(
        second.child.try_wait().unwrap().is_none(),
        "prefix-d detached another shared client"
    );
    second.write("printf 'still-%s\\n' writable\r");
    wait_for_output(&second.output, "still-writable", DEFAULT_TIMEOUT).unwrap();
    wait_for_output(&readonly.output, "still-writable", DEFAULT_TIMEOUT).unwrap();
    assert_eq!(second.size(&env, "remaining-writer-size"), shared);
    second.resize(140, 48);
    wait_for("writer resize received by daemon", DEFAULT_TIMEOUT, || {
        let tree = ipc(&daemon, serde_json::json!({"cmd":"ls_tree"}));
        tree["ls_tree"]["sessions"][0]["clients"]
            .as_array()
            .unwrap()
            .iter()
            .any(|client| {
                client["mode"] == "shared" && client["size"] == serde_json::json!([140, 48])
            })
            .then_some(())
    })
    .unwrap_or_else(|error| {
        panic!(
            "{error}; tree={}, kernel={:?}",
            ipc(&daemon, serde_json::json!({"cmd":"ls_tree"})),
            second.master.get_size().unwrap()
        )
    });
    let mut attempt = 0;
    let mut last_size = small;
    wait_for(
        "remaining writer controls geometry despite smaller readonly viewer",
        DEFAULT_TIMEOUT,
        || {
            attempt += 1;
            let size = second.size(&env, &format!("expanded-{attempt}"));
            last_size = size;
            (size.0 > small.0 && size.1 > small.1).then_some(())
        },
    )
    .unwrap_or_else(|error| {
        panic!(
            "{error}; initial={small:?}, last={last_size:?}, kernel={:?}",
            second.master.get_size().unwrap()
        )
    });
    daemon.assert_alive();
}

#[test]
fn pty_abrupt_ssh_like_disconnect_preserves_same_shell() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "disconnect");
    let mut first = Terminal::attach(&env, "disconnect", "--shared", 100, 32, "xterm-256color");
    first.write("RETAINED=after-hangup\r");
    let before = first.probe(&env, "before-disconnect");
    // SIGKILL closes the client transport with no C_DETACH, as a lost SSH
    // transport would. This is deliberately not reported as a real SSH test.
    let pid = first.child.process_id().unwrap();
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGKILL) }, 0);
    wait_for("abrupt client exit", DEFAULT_TIMEOUT, || {
        first.child.try_wait().unwrap()
    })
    .unwrap();
    daemon.assert_alive();
    let mut second = Terminal::attach(&env, "disconnect", "--shared", 100, 32, "xterm-256color");
    assert_eq!(second.probe(&env, "after-disconnect"), before);
}

#[test]
fn pty_burst_of_key_events_is_not_disconnected_at_queue_capacity() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "burst");
    let mut client = Terminal::attach(&env, "burst", "--shared", 100, 32, "xterm-256color");
    client.write(&format!(
        "padding={}; printf '%s\\n' \"${{#padding}}\" > {}/burst-done\r",
        "x".repeat(512),
        env.root().display()
    ));
    assert_eq!(file_text(&env, "burst-done", &client.output), "512\n");
    assert!(
        client.child.try_wait().unwrap().is_none(),
        "valid input burst disconnected client"
    );
    daemon.assert_alive();
}

#[test]
fn readonly_kill_frame_cannot_terminate_daemon() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "readonly");
    let mut reader = attach_with_mode(&daemon, 80, 24, "readonly");
    let mut writer = attach_with_mode(&daemon, 80, 24, "shared");
    write_msg(&mut reader.stream, 0x04, &[]).unwrap();
    type_text(&mut writer, "printf 'kill-ignored-%s\\n' alive\n").unwrap();
    wait_for_output(&reader.output(), "kill-ignored-alive", DEFAULT_TIMEOUT).unwrap();
    daemon.assert_alive();
}

#[test]
fn inactive_tab_output_larger_than_pty_queue_completes() {
    let env = TestEnv::new();
    let _daemon = spawn_daemon(&env, "background");
    let mut client = Terminal::attach(&env, "background", "--shared", 100, 32, "xterm-256color");
    // Gate output on a file created by the second tab, proving the original
    // tab is inactive before it fills more than any normal kernel PTY queue.
    client.write(&format!("while [ ! -f {0}/go ]; do sleep 0.05; done; head -c 1048576 /dev/zero | tr '\\000' x; printf 'done\\n' > {0}/completed\r", env.root().display()));
    client.prefix('c');
    wait_for("second tab active", DEFAULT_TIMEOUT, || {
        let tree = ipc(&_daemon, serde_json::json!({"cmd": "ls_tree"}));
        (tree["ls_tree"]["sessions"][0]["tabs"]
            .as_array()
            .is_some_and(|tabs| tabs.len() == 2))
        .then_some(())
    })
    .unwrap();
    client.write(&format!(
        "printf 'second\\n' > {}/go\r",
        env.root().display()
    ));
    assert_eq!(file_text(&env, "go", &client.output), "second\n");
    assert_eq!(
        file_text(&env, "completed", &client.output),
        "done\n",
        "background PTY output stalled"
    );
    client.prefix('n');
    wait_for("original tab active again", DEFAULT_TIMEOUT, || {
        let tree = ipc(&_daemon, serde_json::json!({"cmd": "ls_tree"}));
        (tree["ls_tree"]["sessions"][0]["focused_tab"] == 0).then_some(())
    })
    .unwrap();
    client.write("printf 'returned-%s\\n' alive\r");
    wait_for_output(&client.output, "returned-alive", DEFAULT_TIMEOUT).unwrap();
}

#[test]
fn ctl_layout_preserves_shell_pid_and_rejects_pane_count_change() {
    let env = TestEnv::new();
    let daemon = spawn_daemon_args(&env, "layout", &["1", "2"]);
    let mut client = Terminal::attach(&env, "layout", "--shared", 120, 40, "xterm-256color");
    let before = client.probe(&env, "layout-before");
    let output = ctl(&env, &daemon, &["layout", "1/1"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(client.probe(&env, "layout-after"), before);
    let output = ctl(&env, &daemon, &["layout", "1:1:1"]);
    assert!(
        !output.status.success(),
        "pane-count-changing layout was accepted"
    );
    assert_eq!(client.probe(&env, "layout-rejected"), before);
    assert_eq!(
        ipc(&daemon, serde_json::json!({"cmd":"list"}))["panes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn ctrl_w_edits_shell_input_without_closing_the_pane() {
    let env = TestEnv::new();
    let daemon = spawn_daemon(&env, "editing");
    let mut client = Terminal::attach(&env, "editing", "--shared", 100, 32, "xterm-256color");
    client.write(&format!(
        "printf '%s\\n' kept removed\x17> {}/edited\r",
        env.root().display()
    ));
    assert_eq!(file_text(&env, "edited", &client.output), "kept\n");
    assert_eq!(
        ipc(&daemon, serde_json::json!({"cmd":"list"}))["panes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn ctl_too_small_split_is_reported_without_mutation() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "small");
    let mut client = attach_client(&daemon, 3, 3);
    daemon.assert_alive();
    wait_for_output(&client.output(), "\x1b[", DEFAULT_TIMEOUT).unwrap();
    daemon.assert_alive();
    let output = ctl(&env, &daemon, &["split", "h"]);
    assert!(
        !output.status.success(),
        "small split succeeded: {output:?}"
    );
    let list = ipc(&daemon, serde_json::json!({"cmd":"list"}));
    assert_eq!(list["panes"].as_array().unwrap().len(), 1);
    client.send_detach().unwrap();
}

#[test]
fn failed_snapshot_load_keeps_existing_shell_running() {
    let env = TestEnv::new();
    let daemon = spawn_daemon(&env, "rollback");
    let mut client = Terminal::attach(&env, "rollback", "--shared", 100, 32, "xterm-256color");
    let before = client.probe(&env, "rollback-before");
    let path = env.root().join("broken.json");
    assert!(ctl(&env, &daemon, &["save", path.to_str().unwrap()])
        .status
        .success());
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["tabs"][0]["panes"][0]["shell"] = serde_json::json!("/ezpn-test-missing-shell");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let output = ctl(&env, &daemon, &["load", path.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "invalid shell load accepted: {output:?}"
    );
    assert_eq!(client.probe(&env, "rollback-after"), before);
}

#[test]
fn restoring_multiple_tabs_launches_each_command_exactly_once() {
    let env = TestEnv::new();
    let tabs: Vec<_> = (0..2).map(|id| {
        let command = format!("printf 'launched\\n' >> {0}/launch-{id}; while :; do sleep 1; done", env.root().display());
        serde_json::json!({"name":format!("tab-{id}"), "layout":{"root":{"Leaf":{"id":0}},"next_id":1}, "active_pane":0,
            "panes":[{"id":0,"launch":{"command":command}}]})
    }).collect();
    let path = env.root().join("tabs.json");
    let snapshot = serde_json::json!({"version":3,"shell":"/bin/sh","border_style":"rounded","show_status_bar":true,"active_tab":1,"tabs":tabs});
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let mut daemon = spawn_daemon_args(&env, "restore", &["--restore", path.to_str().unwrap()]);
    for id in 0..2 {
        wait_for("restored command executed", DEFAULT_TIMEOUT, || {
            std::fs::read_to_string(env.root().join(format!("launch-{id}")))
                .ok()
                .filter(|s| s.ends_with('\n'))
        })
        .unwrap();
    }
    // Querying live state synchronizes with completion of the daemon startup.
    let response = ipc(&daemon, serde_json::json!({"cmd":"ls_tree"}));
    assert_eq!(response["ok"], true);
    daemon.shutdown();
    for id in 0..2 {
        assert_eq!(
            std::fs::read_to_string(env.root().join(format!("launch-{id}"))).unwrap(),
            "launched\n",
            "tab command launched more than once"
        );
    }
}

#[test]
fn rename_alias_cleanup_preserves_reused_original_session_name() {
    use std::os::unix::fs::MetadataExt;

    let env = TestEnv::new();
    let mut original = spawn_daemon(&env, "A");
    let _original_client = attach_with_mode(&original, 80, 24, "shared");
    let original_path = env.session_socket("A");
    let alias_path = env.session_socket("B");
    let original_metadata = std::fs::symlink_metadata(&original_path).unwrap();
    let original_inode = (original_metadata.dev(), original_metadata.ino());

    let renamed = run_bounded(env.command().args(["rename", "A", "B"]));
    assert!(renamed.status.success(), "rename failed: {renamed:?}");
    assert!(
        !original_path.exists(),
        "rename retained the original locator"
    );
    let alias_metadata = std::fs::symlink_metadata(&alias_path).unwrap();
    assert_eq!((alias_metadata.dev(), alias_metadata.ino()), original_inode);
    original.assert_alive();

    let mut replacement = spawn_daemon(&env, "A");
    assert_ne!(replacement.pid(), original.pid());
    let mut replacement_client = attach_with_mode(&replacement, 80, 24, "shared");
    type_text(
        &mut replacement_client,
        "printf 'replacement-%s\\n' ready\n",
    )
    .unwrap();
    wait_for_output(
        &replacement_client.output(),
        "replacement-ready",
        DEFAULT_TIMEOUT,
    )
    .unwrap();
    let replacement_metadata = std::fs::symlink_metadata(&original_path).unwrap();
    let replacement_inode = (replacement_metadata.dev(), replacement_metadata.ino());
    assert_ne!(replacement_inode, original_inode);

    let killed = kill_session(&env, "B");
    assert!(killed.status.success(), "alias shutdown failed: {killed:?}");
    assert!(
        original.wait_exit().success(),
        "renamed daemon failed to exit"
    );
    assert!(!alias_path.exists(), "renamed daemon left a stale alias");
    replacement.assert_alive();
    let surviving_metadata = std::fs::symlink_metadata(&original_path)
        .expect("alias cleanup removed the replacement daemon's socket");
    assert_eq!(
        (surviving_metadata.dev(), surviving_metadata.ino()),
        replacement_inode
    );

    // A surviving process or old connection is insufficient: its locator
    // must still accept fresh clients and route commands to the live shell.
    let mut reattached = attach_with_mode(&replacement, 80, 24, "shared");
    type_text(&mut reattached, "printf 'surviving-%s\\n' A\n").unwrap();
    wait_for_output(&reattached.output(), "surviving-A", DEFAULT_TIMEOUT).unwrap();
    replacement.assert_alive();
    let sessions = ls(&env);
    assert!(
        sessions.lines().any(|line| line.starts_with("A:")),
        "{sessions}"
    );
    assert!(
        !sessions.lines().any(|line| line.starts_with("B:")),
        "{sessions}"
    );
}

#[test]
fn integration_review_direct_small_split_preserves_shell() {
    let env = TestEnv::new();
    let pair = native_pty_system()
        .openpty(PtySize {
            cols: 12,
            rows: 10,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(ezpn_binary());
    command.env_clear();
    for (key, value) in env.variables() {
        command.env(key, value);
    }
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    command.env("SHELL", "/bin/sh");
    command.env("TERM", "xterm-256color");
    command.env("LC_ALL", "C");
    command.cwd(env.root());
    command.args(["--no-daemon", "1", "1"]);
    let child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let output = Arc::new(Mutex::new(Vec::new()));
    spawn_capture(pair.master.try_clone_reader().unwrap(), output.clone());
    let writer = pair.master.take_writer().unwrap();
    let mut terminal = Terminal {
        master: pair.master,
        writer,
        child,
        output,
    };
    wait_for_output(&terminal.output, "\x1b[?1049h", DEFAULT_TIMEOUT).unwrap();
    terminal.write("RETAINED=direct-split\r");
    let before = terminal.probe(&env, "direct-before");
    assert_eq!(terminal.size(&env, "direct-small"), (7, 10));
    terminal.prefix('%');
    assert_eq!(terminal.probe(&env, "direct-after"), before);
    assert_eq!(terminal.size(&env, "direct-after-size"), (7, 10));
    assert!(terminal.child.try_wait().unwrap().is_none());
    terminal.write(&format!(
        "printf '%s\\n' kept removed\x17> {}/direct-edited\r",
        env.root().display()
    ));
    assert_eq!(file_text(&env, "direct-edited", &terminal.output), "kept\n");
    terminal.write("exit\r");
    terminal.wait_exit();
}

#[test]
fn integration_review_copy_search_enter_accepts_query_before_yank() {
    let env = TestEnv::new();
    let clipboard = env.root().join("test-clipboard");
    let config_dir = env.root().join("config/ezpn");
    std::fs::create_dir_all(&config_dir).unwrap();
    // Capture clipboard output locally; never invoke the host clipboard.
    let config = serde_json::json!({"clipboard": {"copy_command": [
        "/bin/sh", "-c", "cat > \"$1\"", "ezpn-test", clipboard.to_str().unwrap()
    ]}});
    std::fs::write(
        config_dir.join("config.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    let mut daemon = spawn_daemon(&env, "search-enter");
    let mut terminal = Terminal::attach(&env, "search-enter", "--shared", 80, 24, "xterm-256color");
    terminal.write("printf '\\033[2J\\033[H'; printf 'query-%s\\n' target\r");
    wait_for_output(&terminal.output, "query-target", DEFAULT_TIMEOUT).unwrap();
    terminal.prefix('[');
    terminal.write("/query-target\rVy");
    let copied = wait_for("search result copied after Enter", DEFAULT_TIMEOUT, || {
        std::fs::read_to_string(&clipboard)
            .ok()
            .filter(|text| !text.is_empty())
    })
    .unwrap_or_else(|error| panic!("{error}: {}", captured(&terminal.output)));
    assert_eq!(copied.trim_end_matches('\n'), "query-target");
    terminal.write("printf 'search-finished-%s\\n' ready\r");
    wait_for_output(&terminal.output, "search-finished-ready", DEFAULT_TIMEOUT).unwrap();
    daemon.assert_alive();
}

#[test]
fn integration_review_corrupt_inactive_scrollback_executes_no_commands() {
    let env = TestEnv::new();
    let tabs: Vec<_> = (0..2)
        .map(|id| {
            let command = format!(
                "printf 'launched\\n' >> {0}/decode-launch-{id}; while :; do sleep 1; done",
                env.root().display()
            );
            serde_json::json!({
                "name": format!("tab-{id}"),
                "layout": {"root": {"Leaf": {"id": 0}}, "next_id": 1},
                "active_pane": 0,
                "panes": [{"id": 0, "launch": {"command": command}}]
            })
        })
        .collect();
    let mut snapshot = serde_json::json!({
        "version": 3, "shell": "/bin/sh", "border_style": "rounded",
        "show_status_bar": true, "active_tab": 0, "tabs": tabs
    });
    let path = env.root().join("decode-snapshot.json");
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let mut valid = spawn_daemon_args(&env, "decode-valid", &["--restore", path.to_str().unwrap()]);
    for id in 0..2 {
        assert_eq!(
            file_text(&env, &format!("decode-launch-{id}"), &valid.stderr),
            "launched\n"
        );
    }
    valid.shutdown();
    for id in 0..2 {
        std::fs::remove_file(env.root().join(format!("decode-launch-{id}"))).unwrap();
    }

    // Valid bounds but a truncated gzip header in the inactive tab: all
    // blobs must be decoded before even the valid active tab is executed.
    snapshot["tabs"][1]["panes"][0]["scrollback"] = serde_json::json!({
        "encoding": "bincode-gz", "rows": 0, "bytes_uncompressed": 8,
        "payload": [31, 139, 8]
    });
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let rejected = run_bounded(env.command().args([
        "--server",
        "decode-corrupt",
        "--restore",
        path.to_str().unwrap(),
    ]));
    assert!(
        !rejected.status.success(),
        "corrupt snapshot accepted: {rejected:?}"
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("scrollback"),
        "{rejected:?}"
    );
    for id in 0..2 {
        assert!(
            !env.root().join(format!("decode-launch-{id}")).exists(),
            "pane {id} executed before corrupt scrollback was rejected"
        );
    }
    assert!(!env.session_socket("decode-corrupt").exists());
}

#[test]
fn integration_review_borderless_restore_80x24_reserves_footer_before_spawn() {
    let env = TestEnv::new();
    let tabs: Vec<_> = (0..2)
        .map(|id| {
            let command = format!(
                "stty size > {0}/footer-boot-{id}; exec /bin/sh -i",
                env.root().display()
            );
            serde_json::json!({
                "name": format!("footer-{id}"),
                "layout": {"root": {"Leaf": {"id": 0}}, "next_id": 1},
                "active_pane": 0,
                "panes": [{"id": 0, "launch": {"command": command}}]
            })
        })
        .collect();
    let snapshot = serde_json::json!({
        "version": 3, "shell": "/bin/sh", "border_style": "none",
        "show_status_bar": true, "show_tab_bar": true, "active_tab": 0, "tabs": tabs
    });
    let path = env.root().join("footer-snapshot.json");
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let mut daemon = spawn_daemon_args(&env, "footer", &["--restore", path.to_str().unwrap()]);
    for id in 0..2 {
        let initial = file_text(&env, &format!("footer-boot-{id}"), &daemon.stderr);
        let size: Vec<u16> = initial
            .split_whitespace()
            .map(|part| part.parse().unwrap())
            .collect();
        assert_eq!(
            size,
            [21, 80],
            "tab {id} booted with incorrect PTY geometry"
        );
    }
    let mut terminal = Terminal::attach(&env, "footer", "--shared", 80, 24, "xterm-256color");
    // Borderless mode retains a title row, plus tab and status footer rows.
    assert_eq!(terminal.size(&env, "footer-attached"), (21, 80));
    wait_for("tab footer occupies row 23", DEFAULT_TIMEOUT, || {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(&terminal.output.lock().unwrap());
        let footer = parser.screen().rows(0, 80).nth(22).unwrap();
        (footer.contains("footer-0") && footer.contains("footer-1")).then_some(())
    })
    .unwrap_or_else(|error| panic!("{error}: {}", captured(&terminal.output)));
    let readonly = attach_with_mode(&daemon, 40, 12, "readonly");
    wait_for_output(&readonly.output(), "\x1b[", DEFAULT_TIMEOUT).unwrap();
    let mut trace = IntegrationReviewCursorTrace::default();
    vte::Parser::new().advance(&mut trace, &readonly.output().lock().unwrap());
    assert!(
        !trace.positions.is_empty(),
        "readonly frame contained no CUP commands"
    );
    assert!(
        trace
            .positions
            .iter()
            .all(|&(row, col)| row <= 12 && col <= 40),
        "readonly frame addressed outside 40x12: {:?}",
        trace.positions
    );
    assert_eq!(terminal.size(&env, "footer-after-readonly"), (21, 80));
    daemon.assert_alive();
}

#[derive(Default)]
struct IntegrationReviewCursorTrace {
    positions: Vec<(u16, u16)>,
}

impl vte::Perform for IntegrationReviewCursorTrace {
    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if ignore || !intermediates.is_empty() || !matches!(action, 'H' | 'f') {
            return;
        }
        let mut params = params.iter();
        let row = params
            .next()
            .and_then(|p| p.first())
            .copied()
            .unwrap_or(1)
            .max(1);
        let col = params
            .next()
            .and_then(|p| p.first())
            .copied()
            .unwrap_or(1)
            .max(1);
        self.positions.push((row, col));
    }
}
