# 独立仓库推进，mistake-agent 本仓库不修改

mistake-agent 的后续计划原定先在原仓库做 M1 解耦（行为不变）再剥离；经评审改为**不在 mistake-agent 做任何修改**：so-lite-agent 作为新独立仓库推进，通用模块以"参考原源码 + 按通用语义重写"方式直接落地，mistake-agent 保持现状（其"单 crate 不拆分"红线继续有效）。理由：用户明确要求不动原仓库；独立仓库本身是物理边界，M1 的解耦点（system_prompt 注入、ConfigChanged、services 拆分）直接在新 crate 里按通用语义实现，省去双倍改动面。mistake-agent 侧改造**推迟到 v3 再评估**（2026-08-07 确认，M5 不落地）。
