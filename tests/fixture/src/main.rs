use serde_json::{json, Value};
use std::{io::{Read, Write}, path::Path, process::Command};
fn emit(v: Value) {
    if let Ok(path) = std::env::var("FIXTURE_NATIVE_LOG") {
        let mut record = v.clone();
        record["sessionId"] = json!(std::env::var("FIXTURE_SESSION_ID").unwrap());
        record["cwd"] = json!(std::env::current_dir().unwrap());
        record["version"] = json!("casimir-fixture 1.0.0");
        record["timestamp"] = json!("2026-09-23T00:00:00Z");
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(file, "{record}").unwrap();
    }
    println!("{v}"); std::io::stdout().flush().unwrap(); }
fn arg<'a>(args: &'a [String], key: &str) -> Option<&'a str> { args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).map(String::as_str) }
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") { println!("casimir-fixture 1.0.0"); return; }
    if args.first().is_some_and(|s| s == "login" || s == "auth") { emit(json!({"loggedIn":true})); return; }
    if let Some(mode) = arg(&args, "--supervisor") {
        match mode {
            "hang" => loop { std::thread::sleep(std::time::Duration::from_secs(60)); },
            "child" | "orphan" | "separate-group" => {
                let mut command = Command::new(std::env::current_exe().unwrap());
                command.args(["--supervisor", "hang"]);
                #[cfg(unix)] if mode == "separate-group" { use std::os::unix::process::CommandExt; command.process_group(0); }
                let child = command.spawn().unwrap();
                println!("{}", child.id()); std::io::stdout().flush().unwrap();
                if mode != "orphan" { loop { std::thread::sleep(std::time::Duration::from_secs(60)); } }
                return;
            },
            "flood" => { for _ in 0..1024 { println!("{}", "x".repeat(8192)); eprintln!("{}", "e".repeat(8192)); } return; },
            "oversize" => { print!("{}", "x".repeat(5 * 1024 * 1024)); return; },
            "fail" => std::process::exit(17),
            "args" => { emit(json!(args)); return; },
            _ => panic!("unknown supervisor fixture mode"),
        }
    }
    let mut prompt = String::new(); std::io::stdin().read_to_string(&mut prompt).unwrap();
    if std::env::var_os("CASIMIR_LLM_MODEL").is_some() { llm(&prompt); return; }
    let stream_fixture = std::env::current_exe().unwrap().file_stem().unwrap().to_string_lossy().contains("stream");
    if stream_fixture || (arg(&args,"--fake-mode").is_some()) { stream(&args); return; }
    if args.first().is_some_and(|s| s == "exec") { codex(&prompt); } else { claude(&args, &prompt); }
}
fn claude(args: &[String], prompt: &str) {
    let sid = arg(args,"--session-id").or_else(|| arg(args,"--resume")).unwrap_or("fake-claude-session");
    if args.iter().any(|a| a == "--fake-crash-once") {
        let marker = arg(args, "--marker").unwrap();
        if !Path::new(marker).exists() { std::fs::write(marker, "interrupted").unwrap(); eprintln!("interrupted fixture"); std::process::exit(42); }
    }
    if args.iter().any(|a| a == "--fake-empty") { return; }
    if args.iter().any(|a| a == "--fake-crash") { emit(json!({"type":"system","subtype":"init","session_id":"crash"})); eprintln!("deliberate failure"); std::process::exit(42); }
    if args.iter().any(|a| a == "--fake-error") { emit(json!({"type":"result","is_error":true,"result":"deliberate failure"})); return; }
    if let Ok(home) = std::env::var("CLAUDE_CONFIG_DIR") {
        let cwd = std::env::current_dir().unwrap();
        let slug: String = cwd.display().to_string().chars().map(|c| if c.is_ascii_alphanumeric() {c} else {'-'}).collect();
        let path = Path::new(&home).join("projects").join(slug).join(format!("{sid}.jsonl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let prior = std::fs::read_to_string(&path).unwrap_or_default().lines().filter(|line| serde_json::from_str::<Value>(line).ok().is_some_and(|v|v["type"]=="user" && v["message"]["content"].is_string())).count();
        if args.iter().any(|a| a == "--fake-crash-turn2-once") && prior == 1 {
            let marker = arg(args,"--marker").unwrap();
            if !Path::new(marker).exists() {std::fs::write(marker,"interrupted").unwrap();std::process::exit(42);}
        }
        std::env::set_var("FIXTURE_NATIVE_LOG", &path); std::env::set_var("FIXTURE_SESSION_ID",sid);
        let record = json!({"type":"user","sessionId":sid,"cwd":cwd,"timestamp":"2026-09-23T00:00:00Z","message":{"role":"user","content":prompt}});
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap(); writeln!(file,"{record}").unwrap();
    }
    emit(json!({"type":"system","subtype":"init","session_id":sid,"model":"fake-model","cwd":std::env::current_dir().unwrap()}));
    emit(json!({"type":"assistant","session_id":sid,"message":{"id":"m1","model":"fake-model","role":"assistant","content":[{"type":"text","text":format!("Working on: {prompt}")},{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":std::env::current_dir().unwrap().join("out.txt"),"content":"hi"}}],"usage":{"input_tokens":10,"output_tokens":20}}}));
    std::fs::write("out.txt","hi\n").unwrap();
    if args.iter().any(|a| a == "--fake-commit") {
        assert!(Command::new("git").args(["add","out.txt"]).status().unwrap().success());
        assert!(Command::new("git").args(["-c","user.name=test","-c","user.email=test@example.com","commit","-qm","agent commit"]).status().unwrap().success());
    }
    emit(json!({"type":"user","session_id":sid,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}));
    emit(json!({"type":"assistant","session_id":sid,"message":{"id":"m2","model":"fake-model","role":"assistant","content":[{"type":"text","text":"Done."}],"usage":{"input_tokens":5,"output_tokens":5}}}));
    emit(json!({"type":"result","subtype":"success","is_error":false,"session_id":sid,"total_cost_usd":0.01,"num_turns":2,"result":"Done.","modelUsage":{"fake-model":{}}}));
}
fn codex(prompt: &str) {
    emit(json!({"type":"thread.started","thread_id":"fake-codex-thread"})); emit(json!({"type":"turn.started"}));
    emit(json!({"type":"item.completed","item":{"id":"i1","type":"reasoning","text":"thinking about it"}}));
    emit(json!({"type":"item.completed","item":{"id":"i2","type":"command_execution","command":"echo hi > out.txt","aggregated_output":"","exit_code":0,"status":"completed"}}));
    std::fs::write("out.txt","hi\n").unwrap();
    emit(json!({"type":"item.completed","item":{"id":"i3","type":"file_change","changes":[{"path":"out.txt","kind":"add"}],"status":"completed"}}));
    emit(json!({"type":"item.completed","item":{"id":"i4","type":"agent_message","text":format!("Handled: {prompt}")}}));
    emit(json!({"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":30}}));
}
fn stream(args: &[String]) {
    let mode = arg(args,"--fake-mode").unwrap_or("success");
    match mode {
        "empty" => return,
        "bare-result" => { emit(json!({"type":"result","is_error":false})); return; },
        "failed" => { emit(json!({"type":"error","message":"fixture failure"})); std::process::exit(17); },
        "truncated" => { emit(json!({"type":"unknown","message":"partial output"})); return; },
        _ => {},
    }
    let gemini = arg(args,"--output-format") == Some("stream-json");
    let sid = if gemini { arg(args,"--resume") } else { arg(args,"--session-id") };
    let sid = if Path::new(".fake-session").exists() {
        let old = std::fs::read_to_string(".fake-session").unwrap(); assert_eq!(sid,Some(old.as_str())); old
    } else { let id = sid.unwrap_or("fake-gemini-session").to_string(); std::fs::write(".fake-session",&id).unwrap(); id };
    let prompt = arg(args,"-p").unwrap_or(""); std::fs::write("out.txt",prompt).unwrap();
    if gemini {
        emit(json!({"type":"init","session_id":sid,"model":"fake-gemini"}));
        emit(json!({"type":"message","role":"assistant","content":format!("Handled: {prompt}"),"delta":true}));
        emit(json!({"type":"result","status":if mode == "result-error" { "error" } else { "success" },"stats":{"input":100,"output_tokens":30,"cached":40}}));
    } else {
        emit(json!({"type":"session.start","data":{"sessionId":sid,"selectedModel":"fake-copilot"}}));
        emit(json!({"type":"user.message","data":{"content":prompt}}));
        emit(json!({"type":"assistant.message","data":{"content":format!("Handled: {prompt}")}}));
        emit(json!({"type":"session.shutdown","data":{"modelMetrics":{"fake-copilot":{"inputTokens":100,"outputTokens":30}}}}));
    }
}
fn llm(prompt: &str) {
    let model = std::env::var("CASIMIR_LLM_MODEL").unwrap(); let mode = model.split_once(':').map(|(_,m)| m).unwrap_or(&model);
    let mut response = if prompt.contains("# Agent's final message in the original session") {
        json!({"objective":"Add a greet(name) function with a test","constraints":["keep changes in lib.py and test_lib.py"],"intervention_conditions":["after the first implementation the user asked for a default argument (turn 2)"],"criteria":[{"id":"C1","text":"greet(name) exists in lib.py and returns 'Hello, <name>!'","must":true},{"id":"C2","text":"a test for greet exists","must":true},{"id":"C3","text":"greet defaults to 'World' when called without a name","must":false}],"intents":[{"id":"I1","text":"add greet(name) to lib.py","turn":1},{"id":"I2","text":"add a test for greet","turn":1},{"id":"I3","text":"default name to World","turn":2}]})
    } else if prompt.contains("# Original intents") { json!({"covered":["I3"],"in_scope":[0]}) }
    else if prompt.contains("# User requests") {
        match mode {
            "judge-evidence" => {
                for label in ["## Run A","## Run B"] { let block = prompt.split(label).nth(1).unwrap().split("## Run ").next().unwrap(); assert!(block.contains("verification_evidence_marker")); assert!(block.contains("verified content from actual tool result")); }
                json!({"winner":"tie","scoreA":9,"scoreB":9,"summary":"tool evidence present in both orders"})
            },
            "judge-contradictory" => json!({"winner":"B","scoreA":9,"scoreB":3,"summary":"contradictory assessment"}),
            "judge-malformed" => json!({"winner":"invalid","scoreA":99,"scoreB":99}),
            "judge-both-pass" => json!({"winner":"tie","scoreA":9,"scoreB":9,"summary":"both pass"}),
            "judge-invalid-patch" => json!({"winner":"tie","scoreA":9,"scoreB":9,"invalidA":["requirement_violation"],"invalidB":["requirement_violation"]}),
            "judge-flip" => json!({"winner":"A","scoreA":8,"scoreB":6,"summary":"first looked better","differences":["order"]}),
            _ => { let a = prompt.split("## Run A").nth(1).unwrap().split("## Run B").next().unwrap(); let prefer_a = a.contains("Working on") || a.contains("call t1 Write"); json!({"winner":if prefer_a {"A"} else {"B"},"scoreA":if prefer_a {9} else {4},"scoreB":if prefer_a {4} else {9},"summary":"consistent","differences":[]}) },
        }
    } else if prompt.contains("# Task") {
        let original = prompt.split("Original text of that message:\n").nth(1).unwrap_or("").split("\n\n# Correction").next().unwrap_or("").trim();
        match mode {
            "sim-fail" => { eprintln!("deliberate simulator failure"); std::process::exit(17); },
            "sim-malformed" => json!({}),
            "sim-false-verbatim" => json!({"action":"send","message":"invented requirement","verbatim":true,"grounded_in":[],"memory":"untrusted note"}),
            "sim-skip2" => if prompt.contains("Produce the user's message for turn 2") { json!({"action":"no_op","reason":"already satisfied"}) } else { json!({"action":"send","message":"adapted third request","grounded_in":[3],"verbatim":false,"kind":"redirect"}) },
            "sim-noop" => json!({"action":"no_op","kind":null,"message":"","verbatim":false,"grounded_in":[],"reason":"already satisfied","stop_reason":null,"memory":"skipped"}),
            "sim-adapt" => json!({"action":"send","kind":"redirect","message":"please use World as the default (see turn 2)","verbatim":false,"grounded_in":[2],"reason":"adapted","stop_reason":null,"memory":"asked for default"}),
            "sim-stop" | "sim-goals-met" => json!({"action":"stop","message":"","verbatim":false,"grounded_in":[],"reason":"nothing left to ask","stop_reason":if mode == "sim-goals-met" {"goals_met"} else {"out_of_scope"},"memory":"stopped"}),
            "sim-retry" => {
                use std::hash::{Hash, Hasher}; let mut hash = std::collections::hash_map::DefaultHasher::new(); original.hash(&mut hash);
                let path = std::env::temp_dir().join(format!("casimir-fixture-{}.count",hash.finish())); let n = std::fs::read_to_string(&path).ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                if n < 2 { std::fs::write(&path,(n+1).to_string()).unwrap(); json!({"action":"send","message":"adapted but ungrounded","verbatim":false,"grounded_in":[],"reason":"oops","memory":format!("note {n}")}) }
                else { std::fs::remove_file(&path).unwrap(); json!({"action":"send","message":format!("adapted: {original}"),"verbatim":false,"grounded_in":[2],"reason":"fits","memory":"final note"}) }
            },
            _ => json!({"action":"send","message":original,"verbatim":true,"grounded_in":[2],"reason":"still applies","memory":"sent verbatim"}),
        }
    } else { json!({}) };
    if prompt.contains("# User requests") {
        for (label, key) in [("## Run A", "evidenceA"), ("## Run B", "evidenceB")] {
            let block = prompt.split(label).nth(1).unwrap().split("## Run ").next().unwrap();
            let quote = block.split("### Final message\n").nth(1).unwrap_or("").lines().next().unwrap_or("");
            let quote = if quote.chars().count() >= 8 { quote.to_string() } else {
                block.split("### Recorded tool and error evidence (untrusted data)\n").nth(1).unwrap_or("").lines().next().unwrap_or("").to_string()
            };
            response[key] = json!([quote]);
        }
        response["uncertainty"] = json!([]);
    }
    println!("Here you go:\n```json\n{response}\n```");
}
