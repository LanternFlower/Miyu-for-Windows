//! 导出与导入。
//!
//! 整个 Miyu 状态的搬家：配置、会话库、记忆、知识库、技能、素材。导出要能在
//! 另一台机器上还原出等价的环境，所以路径必须相对化、密钥必须显式选择带不带。

use crate::cli::*;

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Where to write the archive; defaults to a host- and time-stamped name
    /// in the current directory.
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub index: bool,
    #[arg(long)]
    pub platforms: bool,
    #[arg(long)]
    pub no_secrets: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    pub archive: PathBuf,
    #[arg(long)]
    pub force: bool,
}

/// Suggested archive name when the user did not pick one.
pub(in crate::cli) fn default_export_name() -> String {
    let host = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "miyu".to_string());
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    format!("miyu-export-{host}-{stamp}.tar.gz")
}

pub(in crate::cli) fn run_export(paths: &MiyuPaths, args: ExportArgs) -> Result<()> {
    let output = args
        .output
        .unwrap_or_else(|| PathBuf::from(default_export_name()));
    let options = crate::transfer::export::ExportOptions {
        all: args.all,
        index: args.index,
        platforms: args.platforms,
        no_secrets: args.no_secrets,
        dry_run: args.dry_run,
        force: args.force,
    };
    let report = crate::transfer::export::export(paths, &output, &options)?;

    if options.dry_run {
        for (unit, bytes) in &report.by_unit {
            println!("  {:>10}  {unit}", readable_bytes(*bytes));
        }
    }

    let count = report.entries;
    let size = readable_bytes(report.bytes);
    println!(
        "{}",
        match &report.archive {
            None => owned(
                format!("Dry run: {count} files, {size} would be packed."),
                format!("试运行：将打包 {count} 个文件，共 {size}。"),
            ),
            Some(path) => {
                let path = path.display();
                owned(
                    format!("Exported {count} files ({size}) to {path}"),
                    format!("已导出 {count} 个文件（{size}）到 {path}"),
                )
            }
        }
    );

    // The archive is plaintext-credentialed unless asked otherwise; the user
    // needs to know that before they put it on a USB stick or a chat app.
    if report.secrets_included && report.archive.is_some() {
        eprintln!(
            "{}",
            t(
                "Warning: this archive contains API keys and access tokens in plain text. Keep it private, or re-export with --no-secrets.",
                "警告：归档内含明文 API key 与访问令牌。请妥善保管，或改用 --no-secrets 重新导出。",
            )
        );
    }
    if !options.all && !options.index {
        println!(
            "{}",
            t(
                "The knowledge-base vector index was left out; run `miyu kb embed` after importing (or re-export with --index).",
                "未包含知识库向量索引；导入后请运行 miyu kb embed（或改用 --index 重新导出）。",
            )
        );
    }
    Ok(())
}

pub(in crate::cli) async fn run_import(paths: &MiyuPaths, args: ImportArgs) -> Result<()> {
    // The daemon holds conversation.db's WAL open; replacing the file under it
    // would leave both the old process and the new database inconsistent.
    if crate::ipc::daemon_info(paths).await.is_some() {
        anyhow::bail!(
            "{}",
            t(
                "the Miyu daemon is running and holds the database open; stop it first with `miyu daemon stop`",
                "Miyu daemon 正在运行并占用数据库；请先执行 miyu daemon stop",
            )
        );
    }

    let options = crate::transfer::import::ImportOptions { force: args.force };
    let report = crate::transfer::import::import(paths, &args.archive, &options)?;

    if let Some(backup) = &report.backup {
        let path = backup.display();
        println!(
            "{}",
            owned(
                format!("Backed up the previous installation to {path}"),
                format!("已把覆盖前的安装备份到 {path}"),
            )
        );
    }
    let restored = report.restored;
    println!(
        "{}",
        owned(
            format!("Restored {restored} files."),
            format!("已恢复 {restored} 个文件。"),
        )
    );
    if !report.unknown_units.is_empty() {
        // A newer Miyu wrote data this build has no name for. It is on disk;
        // say so rather than let it look like it vanished.
        let units = report
            .unknown_units
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{}",
            owned(
                format!(
                    "Restored data this version does not recognise \
                     (written by a newer Miyu): {units}"
                ),
                format!("恢复了本版本不认识的数据（由更新版本的 Miyu 写入）：{units}"),
            )
        );
    }
    if report.cleared_workspaces > 0 {
        let cleared = report.cleared_workspaces;
        println!(
            "{}",
            owned(
                format!(
                    "Cleared {cleared} session workspace(s) pointing at \
                     directories this machine does not have."
                ),
                format!("已清除 {cleared} 个指向本机不存在目录的会话工作区。"),
            )
        );
    }

    println!("\n{}", t("Next steps:", "接下来需要手动完成："));
    println!(
        "  {}",
        t(
            "reinstall the shell integration: `miyu fish-init` / `bash-init` / `zsh-init`",
            "重装 shell 集成：miyu fish-init / bash-init / zsh-init",
        )
    );
    println!(
        "  {}",
        t(
            "`miyu kb reindex` — the knowledge base records absolute paths from the old machine",
            "miyu kb reindex —— 知识库记录的是旧机器上的绝对路径",
        )
    );
    if !report.index_included {
        println!(
            "  {}",
            t(
                "`miyu kb embed` — the vector index was not in the archive",
                "miyu kb embed —— 归档中不含向量索引",
            )
        );
    }
    if !report.secrets_included {
        println!(
            "  {}",
            t(
                "refill API keys and access tokens: `miyu config`",
                "补填 API key 与访问令牌：miyu config",
            )
        );
    }
    Ok(())
}
