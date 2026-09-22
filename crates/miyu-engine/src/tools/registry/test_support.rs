//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/registry/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ToolRegistry {
    /// 测试夹具:按「已加载集合」过滤后的定义清单(旧 lazy 档的目录形状)。生产只走
    /// `definitions` / `stub_definitions`;load_tools 的动态描述随 lazy 档一起退役(09-16)。
    pub fn lazy_definitions(&self, loaded: &BTreeSet<String>) -> Vec<ToolDefinition> {
        let mut definitions = self
            .tools
            .values()
            .filter(|tool| tool.always_loaded || loaded.contains(&tool.name))
            .map(|tool| tool.definition())
            .collect::<Vec<_>>();
        definitions.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        definitions
    }

    pub fn permission(&self, name: &str) -> Result<ToolPermission> {
        let Some(tool) = self.tools.get(name) else {
            bail!("unknown tool: {name}");
        };
        Ok(tool.permission)
    }
}
