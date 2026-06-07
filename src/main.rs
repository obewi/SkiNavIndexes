mod cli;
mod pipeline;

fn main() -> anyhow::Result<()> {
    pipeline::run()
}
