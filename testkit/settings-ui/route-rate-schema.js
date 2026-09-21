// 会话专属配置里「概率主动回复抽样概率」的字段表走查(用户 09-21)。
//
// 字段表是纯数据,不需要浏览器就能验形状;渲染与绑定那层要 Playwright。
// 用法: node testkit/settings-ui/route-rate-schema.js web/settings-schema.js
const fs = require("fs");
global.window = {};
new Function(fs.readFileSync(process.argv[2], "utf8"))();
const fields = window.MiyuSettingsSchema.qq.routes.fields;
const results = [];
const check = (name, ok, detail = "") => { results.push(ok); console.log((ok ? "✅" : "❌") + " " + name + (detail ? "  " + detail : "")); };

const toggle = fields.find((f) => f.key === "probability_reply");
check("开关那条改名成「开关概率主动回复」", toggle && toggle.label === "开关概率主动回复", toggle && toggle.label);
check("开关仍是三档下拉(继承/开/关)", toggle && toggle.kind === "select" && toggle.choices.length === 3);

const rate = fields.find((f) => f.key === "probability_reply_rate");
check("新增「概率主动回复抽样概率」", !!rate && rate.label === "概率主动回复抽样概率", rate && rate.label);
check("是数字输入框", rate && rate.kind === "number");
check("范围 0–1", rate && rate.min === 0 && rate.max === 1, rate && `min=${rate.min} max=${rate.max}`);
check("可留空 = 继承(nullable)", rate && rate.nullable === true);
check("默认不覆盖(default null)", rate && rate.default === null);
check("排在开关下面一行", fields.indexOf(rate) === fields.indexOf(toggle) + 1);

// 插件侧那个被覆盖的值仍在,单位一致(0–1)
const plugin = window.MiyuSettingsSchema.qqPlugins.real_context.groups.flatMap((g) => g.fields || []).find((f) => f.key === "active_judge_probability");
check("插件侧的概率还在且同为 0–1", plugin && plugin.min === 0 && plugin.max === 1, plugin && plugin.label);

const passed = results.filter(Boolean).length;
console.log(`\n${passed}/${results.length} passed`);
process.exit(passed === results.length ? 0 : 1);
