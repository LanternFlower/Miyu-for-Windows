//! 成员回合 / 工具桥的作用域(09-11):工作区落在成员家里,子进程套 Landlock。
//!
//! 会话归成员(归属键非空、账号不是管理员)就给一份 [`MemberScope`]:
//! - 工作区 = `home/<用户>/workspace`(不看会话记录里的 workspace——成员改不了,
//!   也不该把 daemon 的 cwd 当工作区);
//! - 沙盒策略(09-11 用户拍板:沙盒外的读取也禁):可写 {工作区, /tmp, /dev/null,
//!   脚本缓存目录};只读只给跑程序必需的系统目录(/usr /etc /proc …)、内置与已装
//!   脚本目录、miyu 自己的二进制;管理员的家、~/.miyu 的配置与库都摸不到。
//!
//! 管理员、终端、平台回合拿到 `None`:老路不变。

use crate::tools::sandbox::SandboxPolicy;
use crate::web::*;

pub(in crate::web) struct MemberScope {
    pub(in crate::web) workspace: PathBuf,
    pub(in crate::web) policy: Arc<SandboxPolicy>,
}

pub(in crate::web) fn member_scope(
    paths: &MiyuPaths,
    admin_store: &StateStore,
    stores: &StoreRegistry,
    session_id: &str,
) -> Option<MemberScope> {
    let owner = stores.owner_of_session(session_id)?;
    if owner.is_empty() {
        return None;
    }
    let account = admin_store.account_by_id(&owner).ok().flatten()?;
    if account.is_admin() {
        return None;
    }
    let home = paths.user_home_dir(&account.username);
    let workspace = home.join("workspace");
    if let Err(error) = crate::paths::ensure_private_dir(&workspace) {
        tracing::warn!(error = %error, path = %workspace.display(), "member workspace dir");
    }
    // 成员自己的产出目录(artifact 库、生图落盘)也得能读写——artifact 落在
    // `home/<user>/artifacts`(见 tools::artifact::artifacts_root),沙盒不放行就
    // 会「读 artifact:x 报 outside your workspace」(09-11 实测)。先建出来,
    // Landlock 对不存在的授权根是失败关闭。
    let artifacts = home.join("artifacts");
    if let Err(error) = crate::paths::ensure_private_dir(&artifacts) {
        tracing::warn!(error = %error, path = %artifacts.display(), "member artifacts dir");
    }
    let mut read_only: Vec<PathBuf> = [
        "/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/proc", "/sys", "/dev", "/run", "/opt",
        "/var",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    read_only.push(paths.scripts_dir.clone());
    read_only.push(paths.system_scripts_dir.clone());
    // 用 miyu_executable()(剥掉 `/proc/self/exe` 的「 (deleted)」后缀)而不是裸
    // current_exe():部署/重建把二进制换掉后,运行中 daemon 的 current_exe() 读成
    // `.../miyu (deleted)`,那条会被下面 retain(exists) 剔掉→沙盒不放行真二进制的
    // EXECUTE;而 CLI 后端(claude-code 等)起的 `miyu mcp-serve` 用的正是剥过后缀
    // 的真路径,exec 被 Landlock 挡下→claude 报 CONNECTION_CLOSED、MCP 用不了
    // (09-12 坐实的 MCP 桥连不上真凶)。两处取同一条路径。
    if let Ok(exe) = crate::paths::miyu_executable() {
        read_only.push(exe);
    }
    // 成员自己家里的只读产出目录:文档、图片(vision/print_image 读得到自己
    // 生成的图)。会话库、profile 这些不放行,「沙盒外读取也禁」的口径不变。
    read_only.push(home.join("documents"));
    read_only.push(home.join("pictures"));
    // Landlock 对打不开的授权根是失败关闭:不存在的目录先剔掉。
    read_only.retain(|path| path.exists());
    // daemon 的运行时目录(IPC socket core.sock 在里面):成员用 claude-code 等
    // CLI 后端时,CLI 起的 `miyu mcp-serve` 桥要连这个 socket 把工具调用转回
    // daemon 才拿得到 Miyu 工具。CLI 进程被 Landlock 关着,桥子进程继承规则,
    // 不放行这条就连不上、报 CONNECTION_CLOSED(09-11 实测)。桥转的工具调用带
    // 成员 session、在 daemon 侧按成员作用域执行,不越权;裸 IPC 的特权命令
    // (Shutdown 等)按「防君子不防小人」的既定尺度不设防(Landlock 本就不管
    // socket)。runtime 目录只含 miyu 自己的运行时文件,给读写(connect 需要)。
    let runtime_dir = paths.runtime_dir();
    let mut read_write = vec![
        workspace.clone(),
        artifacts,
        PathBuf::from("/tmp"),
        PathBuf::from("/dev/null"),
        paths.cache_dir.clone(),
    ];
    if runtime_dir.exists() {
        read_write.push(runtime_dir);
    }
    let policy = SandboxPolicy {
        read_only,
        read_write,
        home: Some(workspace.clone()),
    };
    Some(MemberScope {
        workspace,
        policy: Arc::new(policy),
    })
}
