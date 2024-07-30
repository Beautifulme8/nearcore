use anyhow::Context;
use near_primitives::types::BlockHeight;
use std::cell::Cell;
use std::path::PathBuf;

#[derive(clap::Parser)]
pub struct MirrorCommand {
    #[clap(subcommand)]
    subcmd: SubCommand,
}

#[derive(clap::Parser)]
enum SubCommand {
    Prepare(PrepareCmd),
    Run(RunCmd),
}

/// initialize a target chain with genesis records from the source chain, and
/// then try to mirror transactions from the source chain to the target chain.
#[derive(clap::Parser)]
struct RunCmd {
    /// source chain home dir
    #[clap(long)]
    source_home: PathBuf,
    /// target chain home dir
    #[clap(long)]
    target_home: PathBuf,
    /// mirror database dir
    #[clap(long)]
    mirror_db_path: Option<PathBuf>,
    /// file containing an optional secret as generated by the
    /// `prepare` command. Must be provided unless --no-secret is given
    #[clap(long)]
    secret_file: Option<PathBuf>,
    /// Equivalent to passing --secret-file <FILE> where <FILE> is a
    /// config that indicates no secret should be used. If this is
    /// given, and --secret-file is also given and points to a config
    /// that does contain a secret, the mirror will refuse to start
    #[clap(long)]
    no_secret: bool,
    /// Start a NEAR node for the source chain, instead of only using
    /// whatever's currently stored in --source-home
    #[clap(long)]
    online_source: bool,
    /// If provided, we will stop after sending transactions coming from
    /// this height in the source chain
    #[clap(long)]
    stop_height: Option<BlockHeight>,
    #[clap(long)]
    config_path: Option<PathBuf>,
    #[clap(long)]
    new_streamer_thread: bool,
}

impl RunCmd {
    fn run(self) -> anyhow::Result<()> {
        openssl_probe::init_ssl_cert_env_vars();
        let runtime = tokio::runtime::Runtime::new().context("failed to start tokio runtime")?;

        let secret = if let Some(secret_file) = &self.secret_file {
            let secret = crate::secret::load(secret_file)
                .with_context(|| format!("Failed to load secret from {:?}", secret_file))?;
            if secret.is_some() && self.no_secret {
                anyhow::bail!(
                    "--no-secret given with --secret-file indicating that a secret should be used"
                );
            }
            secret
        } else {
            if !self.no_secret {
                anyhow::bail!("Please give either --secret-file or --no-secret");
            }
            None
        };

        let system = new_actix_system(runtime);
        system
            .block_on(async move {
                let _subscriber_guard = near_o11y::default_subscriber(
                    near_o11y::EnvFilterBuilder::from_env().finish().unwrap(),
                    &near_o11y::Options::default(),
                )
                .global();
                actix::spawn(crate::run(
                    self.source_home,
                    self.target_home,
                    self.mirror_db_path,
                    secret,
                    self.stop_height,
                    self.online_source,
                    self.config_path,
                    self.new_streamer_thread,
                ))
                .await
            })
            .unwrap()
    }
}

/// Write a new genesis records file where the public keys have been
/// altered so that this binary can sign transactions when mirroring
/// them from the source chain to the target chain
#[derive(clap::Parser)]
struct PrepareCmd {
    /// A genesis records file as output by `neard view-state
    /// dump-state --stream`
    #[clap(long)]
    records_file_in: PathBuf,
    /// Path to the new records file with updated public keys
    #[clap(long)]
    records_file_out: PathBuf,
    /// If this is provided, don't use a secret when mapping public
    /// keys to new source chain private keys. This means that anyone
    /// will be able to sign transactions for the accounts in the
    /// target chain corresponding to accounts in the source chain. If
    /// that is okay, then --no-secret will make the code run slightly
    /// faster, and you won't have to take care to not lose the
    /// secret.
    #[clap(long)]
    no_secret: bool,
    /// Path to the secret. Note that if you don't pass --no-secret,
    /// this secret is required to sign transactions for the accounts
    /// in the target chain corresponding to accounts in the source
    /// chain. This means that if you lose this secret, you will no
    /// longer be able to mirror any traffic.
    #[clap(long)]
    secret_file_out: PathBuf,
}

impl PrepareCmd {
    fn run(self) -> anyhow::Result<()> {
        crate::genesis::map_records(
            &self.records_file_in,
            &self.records_file_out,
            self.no_secret,
            &self.secret_file_out,
        )
    }
}

// copied from neard/src/cli.rs
fn new_actix_system(runtime: tokio::runtime::Runtime) -> actix::SystemRunner {
    // `with_tokio_rt()` accepts an `Fn()->Runtime`, however we know that this function is called exactly once.
    // This makes it safe to move out of the captured variable `runtime`, which is done by a trick
    // using a `swap` of `Cell<Option<Runtime>>`s.
    let runtime_cell = Cell::new(Some(runtime));
    actix::System::with_tokio_rt(|| {
        let r = Cell::new(None);
        runtime_cell.swap(&r);
        r.into_inner().unwrap()
    })
}

impl MirrorCommand {
    pub fn run(self) -> anyhow::Result<()> {
        tracing::warn!(target: "mirror", "the mirror command is not stable, and may be removed or changed arbitrarily at any time");

        match self.subcmd {
            SubCommand::Prepare(r) => r.run(),
            SubCommand::Run(r) => r.run(),
        }
    }
}
