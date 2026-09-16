//! 每一轮模型请求之前的工具目录整备:脚本 / 技能目录热刷新,以及这一轮真正发给
//! 模型的工具定义(按加载模式给全量或桩)。09-17 从 `chat_with_tools` 里抽出。

use crate::agent::*;

impl Agent {
    /// 脚本目录与技能目录按指纹热刷新;指纹没变一次锁都不拿。失败只告警,不中断回合。
    pub(super) async fn refresh_tool_catalogs(&mut self) {
        // 脚本目录刷新独立于 skills.enabled(09-05):此前套在 skills 开关里,
        // 关掉技能就没人再热加载脚本了。指纹没变一次锁都不拿。dev 工具面没有
        // 脚本,不刷。
        if !self.core.dev {
            let current_fingerprint = self.tools.lock().unwrap().script_catalog_fingerprint();
            let config = self.core.config.clone();
            let paths = self.core.paths.clone();
            let refresh = tokio::task::spawn_blocking(move || {
                tools::prepare_script_refresh(current_fingerprint, &config, &paths)
                    .map(|snapshot| (snapshot, paths))
            })
            .await;
            match refresh {
                Ok(Ok((Some(snapshot), paths))) => {
                    let mut registry = self.tools.lock().unwrap();
                    tools::apply_script_refresh(&mut registry, &paths, snapshot);
                    tools::register_script_display_names(&registry);
                }
                Ok(Ok((None, _))) => {}
                Ok(Err(error)) => {
                    tracing::warn!(error = %error, "failed to refresh Miyu script tools")
                }
                Err(error) => {
                    tracing::warn!(error = %error, "Miyu script refresh worker stopped")
                }
            }
        }

        if self.core.config.skills.enabled {
            let current_fingerprint = {
                let registry = self.tools.lock().unwrap();
                registry
                    .contains("load_skill")
                    .then(|| registry.skill_catalog_fingerprint())
            };
            if let Some(current_fingerprint) = current_fingerprint {
                let config = self.core.config.clone();
                let paths = self.core.paths.clone();
                let refresh = tokio::task::spawn_blocking(move || {
                    tools::prepare_skill_refresh(current_fingerprint, &config, &paths)
                        .map(|snapshot| (snapshot, config, paths))
                })
                .await;
                match refresh {
                    Ok(Ok((Some(snapshot), config, paths))) => {
                        let mut registry = self.tools.lock().unwrap();
                        tools::apply_skill_refresh(&mut registry, &config, &paths, snapshot);
                    }
                    Ok(Ok((None, _, _))) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(error = %error, "failed to refresh Miyu skill catalog")
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "Miyu skill catalog worker stopped")
                    }
                }
            }
        }
    }

    /// 这一轮发给模型的工具定义:工具关着或到了轮数上限就一个不给。
    pub(super) fn round_tool_definitions(
        &self,
        tool_limit_reached: bool,
    ) -> Vec<miyu_core::llm::ToolDefinition> {
        if self.core.tools_enabled && !tool_limit_reached {
            let tools = self.tools.lock().unwrap();
            // 有效模式按候选模型池解析(模型级覆盖,任一成员要 full 则整池
            // full)——约束解码型模型吃不下空壳 stub(09-01)。
            if tools::is_stub_loading_mode(&tools::effective_tools_loading_mode(
                &self.core.config,
            )) {
                tools.stub_definitions()
            } else {
                tools.definitions()
            }
        } else {
            Vec::new()
        }
    }
}
