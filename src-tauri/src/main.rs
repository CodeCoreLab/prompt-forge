// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use prompt_forge_core::llm::{body_snippet_str, classify_http_error_u16, openai_endpoint};
use prompt_forge_core::normalize::normalize_output;

fn main() {
    // `--selfcheck`：纯逻辑自检（endpoint 拼接 / 错误分类 / 截断 / 输出规整），供 CI 或手动验证
    if std::env::args().nth(1).as_deref() == Some("--selfcheck") {
        let mut failed = 0;
        let cases = [
            ("https://api.deepseek.com", "https://api.deepseek.com/v1/chat/completions"),
            ("https://api.openai.com/v1", "https://api.openai.com/v1/chat/completions"),
            ("https://x.com/", "https://x.com/v1/chat/completions"),
            (
                "https://proxy.example.com/v1/chat/completions",
                "https://proxy.example.com/v1/chat/completions",
            ),
        ];
        for (input, want) in cases {
            let got = openai_endpoint(input);
            if got == want {
                println!("PASS endpoint {input}");
            } else {
                println!("FAIL endpoint {input}: got {got}");
                failed += 1;
            }
        }
        let r = classify_http_error_u16(502, "x".into()).starts_with("RETRYABLE:")
            && classify_http_error_u16(429, "x".into()).starts_with("RETRYABLE:")
            && !classify_http_error_u16(401, "x".into()).starts_with("RETRYABLE:");
        if r { println!("PASS classify_http_error"); } else { println!("FAIL classify_http_error"); failed += 1; }

        let big = serde_json::json!({"data": "x".repeat(1000)});
        let s = body_snippet_str(&big.to_string());
        if s.len() < 400 && s.ends_with("…(截断)") { println!("PASS body_snippet"); } else { println!("FAIL body_snippet"); failed += 1; }

        // 输出规整管道
        let n_cases: [(&str, Result<String, String>, bool); 9] = [
            ("think块", normalize_output("<think>推理中</think>真实输出"), true),
            ("未闭合think", normalize_output("<think>只想到一半"), false),
            ("围栏", normalize_output("```\n增强结果\n```"), true),
            ("围栏带语言", normalize_output("```text\n增强结果\n```"), true),
            ("双引号", normalize_output("\"增强结果\""), true),
            ("中文引号", normalize_output("“增强结果”"), true),
            ("双层引号", normalize_output("\"\"增强结果\"\""), true),
            ("纯空白", normalize_output("   \n  "), false),
            ("正常文本", normalize_output("普通输出不动"), true),
        ];
        for (name, got, want_ok) in n_cases {
            if got.is_ok() == want_ok {
                println!("PASS normalize {name}");
            } else {
                println!("FAIL normalize {name}: got {got:?}");
                failed += 1;
            }
        }

        // 模板组装：防注入包裹 + 函数式替换语义
        let p = prompt_forge_core::modes::build_prompt("target", "含 $& 和 {input} 字面的原文");
        match p {
            Ok(s) if s.contains("<raw_prompt>含 $& 和 {input} 字面的原文</raw_prompt>")
                && s.contains("保真与语言规则") =>
            {
                println!("PASS build_prompt");
            }
            other => {
                println!("FAIL build_prompt: {other:?}");
                failed += 1;
            }
        }

        if failed == 0 { println!("ALL PASS"); } else { println!("{failed} FAILED"); std::process::exit(1); }
        return;
    }
    prompt_forge_lib::run();
}
