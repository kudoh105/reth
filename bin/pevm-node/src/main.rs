//! Main executable for the PEVM (Parallel EVM) Private Chain Node.
//!
//! This binary is identical to `private-node` but replaces the standard
//! sequential EVM executor with the Block-STM parallel executor from
//! `reth-parallel-evm`.  All consensus logic (Commonware BLS12-381 threshold
//! simplex) remains unchanged.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

use clap::Parser;
use commonware_runtime::{Metrics, Runner};
use eyre::WrapErr as _;
use reth_commonware_consensus::{run_consensus_stack, PrivateNodeHandle};
use reth_ethereum_cli::{chainspec::EthereumChainSpecParser, Cli, Commands};
use reth_node_builder::{NodeHandle, WithLaunchContext};
use reth_node_ethereum::EthereumNode;
use reth_parallel_evm::ParallelEthereumExecutorBuilder;
use reth_rpc_server_types::DefaultRpcModuleValidator;

use std::{sync::Arc, thread};
use tokio::sync::oneshot;
use tracing::{info, info_span};

/// Combined CLI args for the pevm node.
#[derive(Debug, Clone, clap::Args)]
struct PevmNodeArgs {
    #[command(flatten)]
    pub consensus: reth_commonware_consensus::Args,
}

fn main() -> eyre::Result<()> {
    reth_cli_util::sigsegv_handler::install();

    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
    }

    let cli = Cli::<EthereumChainSpecParser, PevmNodeArgs, DefaultRpcModuleValidator>::parse();

    let is_node = matches!(cli.command, Commands::Node(_));

    let (node_handle_tx, node_handle_rx) =
        oneshot::channel::<(PrivateNodeHandle, PevmNodeArgs)>();
    let (consensus_dead_tx, mut consensus_dead_rx) = oneshot::channel();

    let shutdown_token = tokio_util::sync::CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    // Spawn consensus thread.
    let consensus_handle = thread::spawn(move || {
        if !is_node {
            return Ok(());
        }

        let (node, args) = node_handle_rx
            .blocking_recv()
            .wrap_err("channel closed before handle to the execution node could be received")?;

        let consensus_storage = args
            .consensus
            .storage_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("./data/consensus"));

        info_span!("prepare_consensus").in_scope(|| {
            info!(
                path = %consensus_storage.display(),
                "determined directory for consensus data",
            )
        });

        let runtime_config = commonware_runtime::tokio::Config::default()
            .with_tcp_nodelay(Some(true))
            .with_worker_threads(args.consensus.worker_threads)
            .with_storage_directory(consensus_storage)
            .with_catch_panics(true);

        let runner = commonware_runtime::tokio::Runner::new(runtime_config);

        let ret = runner.start(async move |ctx| {
            let ctx = ctx.with_label("consensus");

            let consensus_stack = run_consensus_stack(&ctx, args.consensus, node);
            tokio::pin!(consensus_stack);

            loop {
                tokio::select!(
                    biased;

                    () = shutdown_token_clone.cancelled() => {
                        break Ok(());
                    }

                    ret = &mut consensus_stack => {
                        break ret.and_then(|()| Err(eyre::eyre!(
                            "consensus stack exited unexpectedly"))
                        )
                        .wrap_err("consensus stack failed");
                    }
                )
            }
        });

        let _ = consensus_dead_tx.send(());
        ret
    });

    // Determine concurrency level (one thread per physical CPU core, min 2).
    let concurrency = std::thread::available_parallelism()
        .unwrap_or(std::num::NonZeroUsize::new(4).unwrap());

    // Run Reth EL with the parallel executor.
    cli.run(async move |builder, args: PevmNodeArgs| {
        let NodeHandle { node, node_exit_future } = builder
            .node(EthereumNode::default())
            .apply(|mut builder| {
                // Disable devp2p discovery (we use Commonware P2P).
                builder.config_mut().network.discovery.disable_discovery = true;
                builder
            })
            // Replace the default sequential executor with the Block-STM parallel executor.
            .with_components(
                EthereumNode::components()
                    .executor(ParallelEthereumExecutorBuilder::new(concurrency)),
            )
            .launch()
            .await
            .wrap_err("failed launching execution node")?;

        // Extract handles and send to consensus thread.
        let private_handle = PrivateNodeHandle::new(&node);
        let _ = node_handle_tx.send((private_handle, args));

        // Wait for shutdown.
        tokio::select! {
            _ = node_exit_future => {
                tracing::info!("execution node exited");
            }
            _ = &mut consensus_dead_rx => {
                tracing::info!("consensus node exited");
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received shutdown signal");
            }
        }

        Ok(())
    })
    .wrap_err("execution node failed")?;

    shutdown_token.cancel();

    match consensus_handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(err)) => eprintln!("consensus task exited with error:\n{err:?}"),
        Err(unwind) => std::panic::resume_unwind(unwind),
    }

    Ok(())
}
