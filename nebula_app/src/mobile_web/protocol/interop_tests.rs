use super::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const BROWSER_CLIENT: &str = r#"
const fs = require('node:fs');
const vm = require('node:vm');
const readline = require('node:readline');
globalThis.self = globalThis;
globalThis.crypto = require('node:crypto').webcrypto;
vm.runInThisContext(fs.readFileSync(process.env.PEBREL_DSH_CRYPTO_JS, 'utf8'));
(async () => {
  const lines = readline.createInterface({ input: process.stdin });
  const iterator = lines[Symbol.asyncIterator]();
  const masterKey = process.env.PEBREL_DSH_TEST_KEY;
  const session = 'pebrel-suite-interop';
  const hello = await DshCrypto.clientHello(masterKey, session);
  console.log(JSON.stringify(hello.payload));
  const server = JSON.parse((await iterator.next()).value);
  const cipher = await DshCrypto.acceptServerHello(masterKey, session, hello.clientRandomB64, server);
  for (let index = 0; index < 8; index++) {
    const request = { type: 'http_req', channel: `channel-${index}`, payload: { text: `中文任务\n${index}` } };
    console.log(JSON.stringify(await cipher.seal(request)));
    const sealed = JSON.parse((await iterator.next()).value);
    const response = await cipher.open(sealed);
    if (response.channel !== request.channel || response.payload.text !== request.payload.text) {
      throw new Error('browser response did not match its encrypted request');
    }
  }
  console.log(JSON.stringify({ verified: 8 }));
  lines.close();
})().then(() => process.exit(0), () => process.exit(1));
"#;

#[test]
#[ignore = "requires PEBREL_DSH_CRYPTO_JS pointing to the unmodified Suite browser crypto.js"]
fn original_suite_browser_encrypts_and_decrypts_companion_frames() {
    let source = std::env::var("PEBREL_DSH_CRYPTO_JS").expect("Suite crypto.js fixture path");
    assert!(std::path::Path::new(&source).is_file());
    let executable = std::env::var("PEBREL_DSH_TEST_NODE").unwrap_or_else(|_| "node".to_owned());
    let master_key = random_key().unwrap();
    let mut child = Command::new(executable)
        .args(["-e", BROWSER_CLIENT])
        .env("PEBREL_DSH_CRYPTO_JS", source)
        .env("PEBREL_DSH_TEST_KEY", &master_key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Node browser crypto fixture");
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let hello: ClientHello = serde_json::from_str(&line).expect("original Suite client hello");
    let mut accepted = accept_client_hello(&master_key, hello).unwrap();
    writeln!(input, "{}", accepted.payload).unwrap();
    for index in 0..8 {
        line.clear();
        output.read_line(&mut line).unwrap();
        let sealed: SealedPayload = serde_json::from_str(&line).expect("Suite encrypted request");
        let request = accepted.cipher.open(&sealed).unwrap();
        assert_eq!(request["payload"]["text"], format!("中文任务\n{index}"));
        let response = accepted.cipher.seal(&request).unwrap();
        writeln!(input, "{}", serde_json::to_string(&response).unwrap()).unwrap();
    }
    line.clear();
    output.read_line(&mut line).unwrap();
    let result: Value = serde_json::from_str(&line).expect("browser verification result");
    assert_eq!(result["verified"], 8);
    assert!(child.wait().unwrap().success());
}
