use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let runner_temp = PathBuf::from(env::var("RUNNER_TEMP").expect("RUNNER_TEMP"));
    let hook = runner_temp.join("ci-env-canary-hook.sh");
    fs::write(
        &hook,
        r#"result="$RUNNER_TEMP/ci-env-canary-result.txt"
printf '%s\n' 'next_step_hook=executed' >> "$result"
if [[ "${FAKE_LXC_SSH_KEY:-}" == 'fake-lxc-key-marker' ]]; then
    printf '%s\n' 'lxc_ssh_key=reachable' >> "$result"
fi
if [[ "${FAKE_CF_SECRET:-}" == 'fake-cloudflare-secret-marker' ]]; then
    printf '%s\n' 'cloudflare_secret=reachable' >> "$result"
fi
"#,
    )
    .expect("write hook");

    let github_env = env::var("GITHUB_ENV").expect("GITHUB_ENV");
    let mut github_env = OpenOptions::new()
        .append(true)
        .open(github_env)
        .expect("open GITHUB_ENV");
    writeln!(github_env, "BASH_ENV={}", hook.display()).expect("persist BASH_ENV");

    println!("cargo:warning=installed fake-secret canary hook for the next step");
}
