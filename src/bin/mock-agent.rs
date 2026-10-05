use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

fn emit(value: Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}

fn gate_mock(scenario: &str) {
    let mut step = 0;
    for line in io::stdin().lock().lines() {
        let command: Value = serde_json::from_str(&line.unwrap()).unwrap();
        match command["type"].as_str().unwrap() {
            "get_commands" => {
                let commands = if scenario == "gate-missing" {
                    json!([])
                } else {
                    json!([{"name":"inhibitor-gate-v1","description":"inhibitor-tool-gate/v1","source":"extension"}])
                };
                emit(
                    json!({"type":"response","id":command["id"],"command":"get_commands","success":true,"data":{"commands":commands}}),
                );
                continue;
            }
            "prompt" => {
                emit(
                    json!({"type":"response","id":command["id"],"command":"prompt","success":true,"data":{"disposition":"started"}}),
                );
                emit(json!({"type":"agent_start"}));
                if matches!(scenario, "gate-no-decision" | "gate-preflight-error") {
                    emit(
                        json!({"type":"tool_execution_end","toolCallId":"unadmitted","toolName":"write","isError":scenario == "gate-preflight-error"}),
                    );
                    continue;
                }
            }
            "extension_ui_response" => {
                let allowed = command["confirmed"].as_bool().unwrap();
                let id = format!("gate-tool-{step}");
                let name = if step == 2 { "write" } else { "read" };
                if scenario != "gate-unconfirmed" {
                    emit(
                        json!({"type":"extension_ui_request","method":"notify","message":json!({"protocol":"inhibitor-tool-gate/v1","tool_call_id":id,"tool_name":name,"allowed":if scenario == "gate-mismatch" { !allowed } else { allowed }}).to_string()}),
                    );
                }
                emit(
                    json!({"type":"tool_execution_end","toolCallId":id,"toolName":name,"isError":if scenario == "gate-deny-success" && !allowed { false } else { !allowed || step < 2 }}),
                );
                step += 1;
                if step == 3 || (step == 2 && scenario == "gate-no-next") {
                    emit(json!({"type":"agent_settled"}));
                    continue;
                }
            }
            _ => panic!("unexpected gate test command"),
        }
        let id = if scenario == "gate-repeat" && step == 1 {
            "gate-tool-0".to_owned()
        } else {
            format!("gate-tool-{step}")
        };
        let request_id = if scenario == "gate-repeat-request" {
            "fixed".to_owned()
        } else {
            format!("request-{step}")
        };
        emit(
            json!({"type":"tool_execution_start","toolCallId":id,"toolName":if step == 2 { "write" } else { "read" }}),
        );
        emit(
            json!({"type":"extension_ui_request","method":"confirm","id":request_id,"title":if scenario == "gate-foreign-dialog" { "Another extension asks for approval" } else { "inhibitor-tool-gate/v1" },"message":json!({"tool_call_id":id,"tool_name":if step == 2 { "write" } else { "read" }}).to_string()}),
        );
    }
}

fn rpc(scenario: &str) {
    if scenario == "rpc-no-stdin" {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    for line in io::stdin().lock().lines() {
        let command: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let kind = command["type"].as_str().unwrap();
        let mut response =
            json!({"type":"response", "id":command["id"], "command":kind, "success":true});
        match kind {
            "prompt" => {
                response["data"] = json!({"disposition":"started"});
                emit(response);
                emit(json!({"type":"agent_start"}));
                for id in if scenario == "rpc-natural" {
                    vec!["one"]
                } else {
                    vec!["one", "one", "two"]
                } {
                    emit(
                        json!({"type":"tool_execution_end", "toolCallId":id, "toolName":"read", "isError":true}),
                    );
                }
                emit(json!({"type":"agent_end", "willRetry":true}));
                if scenario == "rpc-premature" {
                    return;
                }
                if ["rpc-natural", "rpc-record"].contains(&scenario) {
                    emit(json!({"type":"agent_settled"}));
                }
            }
            "clear_queue" => {
                if ["rpc-ignore", "rpc-ended-only"].contains(&scenario) {
                    continue;
                }
                if scenario == "rpc-race" {
                    emit(
                        json!({"type":"tool_execution_start", "toolCallId":"in-flight", "toolName":"read"}),
                    );
                }
                emit(response);
            }
            "abort" => {
                if scenario == "rpc-reject" {
                    response["success"] = json!(false);
                    response["error"] = json!("mock abort rejected");
                } else {
                    emit(json!({"type":"agent_settled"}));
                }
                emit(response);
            }
            "get_state" => {
                response["data"] = json!({"isStreaming": scenario == "rpc-active", "isCompacting": false, "pendingMessageCount":0});
                if scenario == "rpc-wrong-id" {
                    response["id"] = json!("foreign-response");
                }
                if scenario == "rpc-wrong-command" {
                    response["command"] = json!("different-command");
                }
                emit(response);
            }
            _ => panic!("unexpected test command {kind}"),
        }
    }
    if scenario == "rpc-bad-exit" {
        std::process::exit(7);
    }
}

fn main() {
    match std::env::args().nth(1).as_deref().unwrap_or("repeat") {
        scenario if scenario.starts_with("gate-") => gate_mock(scenario),
        scenario if scenario.starts_with("rpc-") => rpc(scenario),
        "normal" => {
            println!("INFO starting task");
            eprintln!("INFO diagnostic stream is not automatically an error");
            println!("INFO task completed");
        }
        "repeat" => {
            println!("INFO starting task");
            for _ in 0..3 {
                eprintln!("ERROR simulated upstream unavailable");
            }
            println!("INFO finished; the observer did not stop this process");
        }
        "fail" => {
            eprintln!("ERROR simulated child failure");
            std::process::exit(7);
        }
        "long-line" => {
            println!("{}ERROR", "x".repeat(70 * 1024));
            print!("final line without newline");
            io::stdout().flush().unwrap();
        }
        "pi-invalid-utf8" => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(b"{\"type\":\"tool_execution_end\",\"toolCallId\":\"bad-\xff\",\"toolName\":\"read\",\"isError\":true}\n").unwrap();
            stdout.write_all(b"{\"type\":\"tool_execution_end\",\"toolCallId\":\"valid\",\"toolName\":\"read\",\"isError\":true}\n").unwrap();
        }
        scenario @ ("pi" | "pi-malformed") => {
            println!(r#"{{"type":"session","id":"test"}}"#);
            if scenario == "pi-malformed" {
                println!("invalid protocol line");
            }
            // Neither ordinary result text nor JSON on stderr is a failed tool event.
            println!(
                r#"{{"type":"tool_execution_end","toolCallId":"ok","toolName":"read","isError":false,"result":{{"content":[{{"text":"ERROR"}}]}}}}"#
            );
            eprintln!(
                r#"{{"type":"tool_execution_end","toolCallId":"stderr","toolName":"read","isError":true}}"#
            );
            for id in ["first", "first", "second", "third"] {
                println!(
                    "{}",
                    serde_json::json!({"type":"tool_execution_end", "toolCallId":id, "toolName":"read", "isError":true})
                );
            }
            println!(r#"{{"type":"agent_settled"}}"#);
        }
        other => {
            eprintln!("unknown scenario: {other}; use normal, repeat, fail, or long-line");
            std::process::exit(2);
        }
    }
}
