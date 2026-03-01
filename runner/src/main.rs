use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use futures::TryStreamExt;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{ObjectStore, PutPayload};

#[derive(Parser)]
#[command(name = "bench-cache", about = "Cache benchmark data in GCS")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download cached benchmark data from GCS. Exits non-zero on cache miss.
    Download {
        /// Benchmark name (e.g. tpch, clickbench_1, tpcds)
        benchmark: String,
        /// GCS bucket name
        #[arg(long)]
        bucket: String,
        /// Local data directory
        #[arg(long)]
        data_dir: PathBuf,
    },
    /// Upload benchmark data to GCS for future cache hits.
    Upload {
        /// Benchmark name
        benchmark: String,
        /// GCS bucket name
        #[arg(long)]
        bucket: String,
        /// Local data directory
        #[arg(long)]
        data_dir: PathBuf,
    },
}

const MARKER: &str = ".complete";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Download {
            benchmark,
            bucket,
            data_dir,
        } => {
            let store = build_store(&bucket)?;
            let prefix = object_store::path::Path::from(format!("benchdata/{benchmark}"));

            // Check for completion marker
            let marker_path = prefix.child(MARKER);
            if store.head(&marker_path).await.is_err() {
                eprintln!("No cached data for {benchmark}");
                std::process::exit(1);
            }

            // List and download all objects under the prefix
            let mut list_stream = store.list(Some(&prefix));
            while let Some(meta) = list_stream.try_next().await? {
                if meta.location == marker_path {
                    continue;
                }
                // Strip prefix to get relative path
                let rel = meta
                    .location
                    .as_ref()
                    .strip_prefix(prefix.as_ref())
                    .unwrap_or(meta.location.as_ref())
                    .trim_start_matches('/');
                let local_path = data_dir.join(rel);
                if let Some(parent) = local_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let bytes = store.get(&meta.location).await?.bytes().await?;
                std::fs::write(&local_path, &bytes)?;
                eprintln!("  downloaded: {rel}");
            }

            eprintln!("Cache download complete for {benchmark}");
        }
        Command::Upload {
            benchmark,
            bucket,
            data_dir,
        } => {
            let store = build_store(&bucket)?;
            let prefix = object_store::path::Path::from(format!("benchdata/{benchmark}"));

            // Upload all files under data_dir
            upload_dir(&store, &prefix, &data_dir, &data_dir).await?;

            // Write completion marker
            let marker_path = prefix.child(MARKER);
            store
                .put(&marker_path, PutPayload::from_static(b"ok"))
                .await?;

            eprintln!("Cache upload complete for {benchmark}");
        }
    }

    Ok(())
}

fn build_store(bucket: &str) -> Result<impl ObjectStore, object_store::Error> {
    GoogleCloudStorageBuilder::new()
        .with_bucket_name(bucket)
        .build()
}

async fn upload_dir(
    store: &impl ObjectStore,
    prefix: &object_store::path::Path,
    base: &Path,
    dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            Box::pin(upload_dir(store, prefix, base, &path)).await?;
        } else {
            let rel = path.strip_prefix(base)?;
            let key = prefix.child(rel.to_string_lossy().as_ref());
            let data = std::fs::read(&path)?;
            store.put(&key, PutPayload::from(data)).await?;
            eprintln!("  uploaded: {}", rel.display());
        }
    }
    Ok(())
}
