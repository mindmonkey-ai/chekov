use std::process::ExitCode;

fn main() -> ExitCode {
    match chekov::cli::run() {
        Ok(code) => code,
        Err(err) => {
            // §C.3: anyhow at the binary boundary only — one readable chain.
            let err = anyhow::Error::new(err);
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
