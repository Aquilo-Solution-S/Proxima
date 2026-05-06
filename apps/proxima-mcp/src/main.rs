use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match proxima_mcp::run(std::env::args().skip(1)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) if err.is_help() => {
            println!("{}", proxima_mcp::USAGE);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
