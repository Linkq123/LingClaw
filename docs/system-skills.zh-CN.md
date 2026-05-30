# 系统技能目录

本文档记录 `docs/reference/skills` 下内置系统技能的说明。每一项包含 skill id、显示名称、`SKILL.md` 路径和中文描述。

## Anthropic 技能

| Skill ID | 名称 | 描述 | 路径 |
| --- | --- | --- | --- |
| `anthropics/algorithmic-art` | `algorithmic-art` | 使用 p5.js、种子随机性和交互式参数探索创建算法艺术。适用于用户请求用代码创作艺术、生成艺术、算法艺术、流场或粒子系统的场景；应创作原创作品，避免复制现有艺术家的作品。 | `docs/reference/skills/anthropics/algorithmic-art/SKILL.md` |
| `anthropics/brand-guidelines` | `brand-guidelines` | 将 Anthropic 官方品牌颜色和字体应用到需要 Anthropic 视觉风格的产物中。适用于涉及品牌色、风格指南、视觉格式或公司设计标准的任务。 | `docs/reference/skills/anthropics/brand-guidelines/SKILL.md` |
| `anthropics/canvas-design` | `canvas-design` | 使用设计方法创建精美的 `.png` 和 `.pdf` 静态视觉作品。适用于海报、艺术作品、设计稿或其他静态作品；应生成原创设计，避免复制现有艺术家的作品。 | `docs/reference/skills/anthropics/canvas-design/SKILL.md` |
| `anthropics/claude-api` | `claude-api` | 构建、调试和优化 Claude API / Anthropic SDK 应用，并要求此类应用包含 prompt caching。也用于 Claude 模型版本迁移和退役模型替换。适用于代码导入 `anthropic` 或 `@anthropic-ai/sdk`、用户询问 Claude API / Anthropic SDK / Managed Agents、或需要调整缓存、thinking、compaction、tool use、batch、files、citations、memory、Opus/Sonnet/Haiku 等 Claude 功能和模型的场景。 | `docs/reference/skills/anthropics/claude-api/SKILL.md` |
| `anthropics/doc-coauthoring` | `doc-coauthoring` | 通过结构化流程协助用户共同撰写文档。适用于文档、提案、技术规范、决策文档等结构化内容写作，帮助转移上下文、迭代内容并验证文档对读者有效。 | `docs/reference/skills/anthropics/doc-coauthoring/SKILL.md` |
| `anthropics/docx` | `docx` | 处理 Word 文档 `.docx` 的创建、读取、编辑和操作。适用于 Word 文档、专业报告、目录、标题、页码、信头、图片替换、查找替换、修订、评论和内容整理等任务；不用于 PDF、电子表格、Google Docs 或无关的一般代码任务。 | `docs/reference/skills/anthropics/docx/SKILL.md` |
| `anthropics/frontend-design` | `frontend-design` | 创建有辨识度、生产级、设计质量高的前端界面。适用于网站、落地页、仪表盘、React 组件、HTML/CSS 布局、Web UI 美化等任务，并避免通用的 AI 风格。 | `docs/reference/skills/anthropics/frontend-design/SKILL.md` |
| `anthropics/internal-comms` | `internal-comms` | 用公司偏好的格式撰写各类内部沟通内容。适用于状态报告、领导层更新、3P 更新、公司简报、FAQ、事故报告、项目更新等内部沟通材料。 | `docs/reference/skills/anthropics/internal-comms/SKILL.md` |
| `anthropics/mcp-builder` | `mcp-builder` | 指导创建高质量 MCP 服务器，让 LLM 通过设计良好的工具与外部服务交互。适用于用 Python FastMCP 或 Node/TypeScript MCP SDK 集成外部 API 或服务。 | `docs/reference/skills/anthropics/mcp-builder/SKILL.md` |
| `anthropics/pdf` | `pdf` | 处理 PDF 文件相关任务，包括读取、提取文本和表格、合并、拆分、旋转、加水印、创建 PDF、填写表单、加密解密、提取图片，以及对扫描 PDF 执行 OCR。用户提到 `.pdf` 文件或要求产出 PDF 时使用。 | `docs/reference/skills/anthropics/pdf/SKILL.md` |
| `anthropics/pptx` | `pptx` | 处理所有涉及 `.pptx` 的任务，包括创建演示文稿、读取或提取文本、编辑更新、合并拆分幻灯片、处理模板、布局、演讲者备注和评论。用户提到 deck、slides、presentation 或 `.pptx` 文件时使用。 | `docs/reference/skills/anthropics/pptx/SKILL.md` |
| `anthropics/skill-creator` | `skill-creator` | 创建新技能、修改和优化现有技能，并衡量技能表现。适用于从零创建技能、编辑技能、运行评测、进行性能基准和方差分析，以及优化技能描述以提升触发准确率。 | `docs/reference/skills/anthropics/skill-creator/SKILL.md` |
| `anthropics/slack-gif-creator` | `slack-gif-creator` | 创建适合 Slack 使用的动画 GIF，提供约束、验证工具和动画概念。适用于用户请求制作 Slack GIF 的场景。 | `docs/reference/skills/anthropics/slack-gif-creator/SKILL.md` |
| `anthropics/theme-factory` | `theme-factory` | 为幻灯片、文档、报告、HTML 落地页等产物应用主题样式。提供 10 套预设颜色和字体主题，也可按需生成新主题。 | `docs/reference/skills/anthropics/theme-factory/SKILL.md` |
| `anthropics/web-artifacts-builder` | `web-artifacts-builder` | 使用现代前端技术创建复杂、多组件的 claude.ai HTML artifact，包括 React、Tailwind CSS 和 shadcn/ui。适用于需要状态管理、路由或 shadcn/ui 组件的复杂 artifact，不用于简单单文件 HTML/JSX。 | `docs/reference/skills/anthropics/web-artifacts-builder/SKILL.md` |
| `anthropics/webapp-testing` | `webapp-testing` | 使用 Playwright 与本地 Web 应用交互并测试。支持验证前端功能、调试 UI 行为、捕获浏览器截图和查看浏览器日志。 | `docs/reference/skills/anthropics/webapp-testing/SKILL.md` |
| `anthropics/xlsx` | `xlsx` | 当电子表格文件是主要输入或输出时使用。适用于打开、读取、编辑或修复 `.xlsx`、`.xlsm`、`.csv`、`.tsv`，创建新表格，清洗和重构表格数据，生成公式、格式、图表，或在表格格式之间转换。 | `docs/reference/skills/anthropics/xlsx/SKILL.md` |

## OpenAI 技能

| Skill ID | 名称 | 描述 | 路径 |
| --- | --- | --- | --- |
| `openai/aspnet-core` | `aspnet-core` | 按当前官方 .NET Web 开发指导构建、审查、重构或设计 ASP.NET Core Web 应用。适用于 Blazor Web Apps、Razor Pages、MVC、Minimal APIs、控制器 Web API、SignalR、gRPC、中间件、依赖注入、配置、认证授权、测试、性能、部署和升级。 | `docs/reference/skills/openai/aspnet-core/SKILL.md` |
| `openai/chatgpt-apps` | `chatgpt-apps` | 构建、脚手架化、重构和排查 ChatGPT Apps SDK 应用，这类应用通常由 MCP server 和 widget UI 组成。适用于设计工具、注册 UI resources、接入 Apps bridge 或兼容 API、配置 metadata、CSP、domain settings，以及生成符合文档的项目脚手架。 | `docs/reference/skills/openai/chatgpt-apps/SKILL.md` |
| `openai/cli-creator` | `cli-creator` | 基于 API 文档、OpenAPI 规范、curl 示例、SDK、Web 应用、管理工具或本地脚本，为 Codex 构建可组合 CLI。适用于需要创建可跨仓库运行、提供稳定 JSON、管理认证并可配套 skill 的命令行工具。 | `docs/reference/skills/openai/cli-creator/SKILL.md` |
| `openai/cloudflare-deploy` | `cloudflare-deploy` | 使用 Workers、Pages 和相关平台服务将应用或基础设施部署到 Cloudflare。适用于用户要求在 Cloudflare 上部署、托管、发布或设置项目。 | `docs/reference/skills/openai/cloudflare-deploy/SKILL.md` |
| `openai/define-goal` | `define-goal` | 在开始工作前帮助用户定义具体、可衡量的目标。适用于使用 goal 工具、创建目标、设定 objective、澄清成功标准，或把模糊意图转成量化结果；仅用于目标创建和目标细化。 | `docs/reference/skills/openai/define-goal/SKILL.md` |
| `openai/figma` | `figma` | 使用 Figma MCP server 获取设计上下文、截图、变量和资产，并将 Figma 节点转成生产代码。适用于 Figma URL、node ID、设计转代码实现，以及 Figma MCP 设置和排错。 | `docs/reference/skills/openai/figma/SKILL.md` |
| `openai/figma-code-connect-components` | `figma-code-connect-components` | 使用 Code Connect 映射工具将 Figma 设计组件连接到代码组件。适用于 code connect、组件映射、设计与实现关联等任务；如果需要写入 Figma 画布，应使用 `figma-use`。 | `docs/reference/skills/openai/figma-code-connect-components/SKILL.md` |
| `openai/figma-create-design-system-rules` | `figma-create-design-system-rules` | 为用户代码库生成自定义设计系统规则。适用于创建项目设计规则、定制 Figma-to-code 工作流约定等任务，并需要 Figma MCP server 连接。 | `docs/reference/skills/openai/figma-create-design-system-rules/SKILL.md` |
| `openai/figma-create-new-file` | `figma-create-new-file` | 创建新的空白 Figma 或 FigJam 文件。适用于用户希望新建设计文件，或在调用 `use_figma` 前需要新文件的场景；必要时会通过 whoami 处理计划权限。 | `docs/reference/skills/openai/figma-create-new-file/SKILL.md` |
| `openai/figma-generate-design` | `figma-generate-design` | 与 `figma-use` 配合，将应用页面、视图或多区块布局转换为 Figma 设计。适用于从代码或描述在 Figma 中创建或更新完整页面、屏幕或视图，并通过设计系统组件、变量和样式增量组装。 | `docs/reference/skills/openai/figma-generate-design/SKILL.md` |
| `openai/figma-generate-library` | `figma-generate-library` | 从代码库构建或更新专业级 Figma 设计系统。适用于创建变量和 token、组件库、明暗主题、基础文档，以及弥合代码和 Figma 之间的差距；应与 `figma-use` 一起加载。 | `docs/reference/skills/openai/figma-generate-library/SKILL.md` |
| `openai/figma-implement-design` | `figma-implement-design` | 将 Figma 设计转成具备 1:1 视觉还原度的生产应用代码。适用于根据 Figma 文件实现 UI 代码、生成组件、实现设计稿或匹配 Figma 规格；写入 Figma 画布时使用 `figma-use`。 | `docs/reference/skills/openai/figma-implement-design/SKILL.md` |
| `openai/figma-use` | `figma-use` | 每次调用 `use_figma` 工具前必须先加载的前置技能。适用于需要在 Figma 文件上下文执行 JavaScript 的写操作或特殊读操作，例如创建、编辑、删除节点，设置变量或 token，构建组件和 variants，修改 auto-layout 或 fills，绑定变量，或程序化检查文件结构。 | `docs/reference/skills/openai/figma-use/SKILL.md` |
| `openai/gh-address-comments` | `gh-address-comments` | 使用 `gh` CLI 处理当前分支对应 GitHub PR 上的 review 或 issue 评论。使用前先验证 `gh` 登录状态，未登录时提示用户认证。 | `docs/reference/skills/openai/gh-address-comments/SKILL.md` |
| `openai/gh-fix-ci` | `gh-fix-ci` | 调试或修复 GitHub Actions 中失败的 PR checks。使用 `gh` 查看 checks 和日志，总结失败上下文，先给出修复计划，只有在用户明确批准后才实施；外部 CI 提供商视为范围外。 | `docs/reference/skills/openai/gh-fix-ci/SKILL.md` |
| `openai/hatch-pet` | `hatch-pet` | 创建、修复、验证、视觉 QA 并打包 Codex 兼容的动画宠物和 spritesheet。适用于基于角色图、生成图、公司或客户品牌线索、视觉参考制作自定义宠物或完整 8x9 动画 atlas；会组合 `imagegen` 并使用脚本生成确定性 spritesheet。 | `docs/reference/skills/openai/hatch-pet/SKILL.md` |
| `openai/imagegen` | `imagegen` | 在任务需要 AI 生成或编辑位图视觉资源时使用，例如照片、插画、纹理、sprites、mockups、透明背景 cutouts。适用于创建新图片、转换现有图片或基于参考生成视觉变体；不适合直接编辑 SVG、图标系统或用 HTML/CSS/canvas 实现的视觉。 | `docs/reference/skills/openai/imagegen/SKILL.md` |
| `openai/jupyter-notebook` | `jupyter-notebook` | 创建、脚手架化或编辑 Jupyter notebook `.ipynb`，用于实验、探索或教程。优先使用内置模板，并运行 `new_notebook.py` 生成干净的起始 notebook。 | `docs/reference/skills/openai/jupyter-notebook/SKILL.md` |
| `openai/linear` | `linear` | 管理 Linear 中的问题、项目和团队工作流。适用于读取、创建或更新 Linear tickets。 | `docs/reference/skills/openai/linear/SKILL.md` |
| `openai/migrate-to-codex` | `migrate-to-codex` | 将支持的指令文件、skills、agents 和 MCP 配置迁移到 Codex 的项目级或全局文件中。 | `docs/reference/skills/openai/migrate-to-codex/SKILL.md` |
| `openai/netlify-deploy` | `netlify-deploy` | 使用 Netlify CLI (`npx netlify`) 将 Web 项目部署到 Netlify。适用于在 Netlify 上部署、托管、发布或关联站点和仓库，包括 preview 和 production deploy。 | `docs/reference/skills/openai/netlify-deploy/SKILL.md` |
| `openai/notion-knowledge-capture` | `notion-knowledge-capture` | 将对话和决策整理为结构化 Notion 页面。适用于把聊天、笔记转成 wiki、how-to、决策记录或 FAQ，并建立合适链接。 | `docs/reference/skills/openai/notion-knowledge-capture/SKILL.md` |
| `openai/notion-meeting-intelligence` | `notion-meeting-intelligence` | 结合 Notion 上下文和 Codex 研究准备会议材料。适用于收集背景、起草议程和预读材料，并按参会者定制内容。 | `docs/reference/skills/openai/notion-meeting-intelligence/SKILL.md` |
| `openai/notion-research-documentation` | `notion-research-documentation` | 在 Notion 中跨来源研究并综合成结构化文档。适用于从多个 Notion 来源收集信息，产出带引用的 brief、对比或报告。 | `docs/reference/skills/openai/notion-research-documentation/SKILL.md` |
| `openai/notion-spec-to-implementation` | `notion-spec-to-implementation` | 将 Notion 规格转成实现计划、任务和进度跟踪。适用于实现 PRD 或 feature spec，并由此创建 Notion 计划和任务。 | `docs/reference/skills/openai/notion-spec-to-implementation/SKILL.md` |
| `openai/openai-docs` | `openai-docs` | 当用户询问如何使用 OpenAI 产品或 API、Codex 本身、Codex 使用场景、最新模型选择、模型升级或 prompt 升级时使用。非 Codex 文档问题优先使用 OpenAI docs MCP 工具，Codex 通用知识优先使用 Codex manual helper，fallback 浏览限制在 OpenAI 官方域名。 | `docs/reference/skills/openai/openai-docs/SKILL.md` |
| `openai/pdf` | `pdf` | 当任务涉及读取、创建或审查 PDF 且渲染和布局很重要时使用。优先通过渲染页面进行视觉检查，并使用 `reportlab`、`pdfplumber`、`pypdf` 等 Python 工具生成和提取内容。 | `docs/reference/skills/openai/pdf/SKILL.md` |
| `openai/playwright` | `playwright` | 当任务需要从终端自动化真实浏览器时使用，包括导航、填写表单、快照、截图、数据提取和 UI 流程调试，可用 `playwright-cli` 或内置 wrapper script。 | `docs/reference/skills/openai/playwright/SKILL.md` |
| `openai/playwright-interactive` | `playwright-interactive` | 通过 `js_repl` 进行持久浏览器和 Electron 交互，用于快速迭代式 UI 调试。 | `docs/reference/skills/openai/playwright-interactive/SKILL.md` |
| `openai/plugin-creator` | `plugin-creator` | 创建和脚手架化 Codex plugin 目录，包括必需的 `.codex-plugin/plugin.json`、可选 plugin 文件夹和发布或测试前可编辑的占位内容。适用于创建本地 plugin、补充 plugin 结构，或生成/更新 repo-root `.agents/plugins/marketplace.json` 条目。 | `docs/reference/skills/openai/plugin-creator/SKILL.md` |
| `openai/render-deploy` | `render-deploy` | 通过分析代码库、生成 `render.yaml` Blueprint 和提供 Dashboard deeplink，将应用部署到 Render。适用于在 Render 云平台部署、托管、发布或设置应用。 | `docs/reference/skills/openai/render-deploy/SKILL.md` |
| `openai/screenshot` | `screenshot` | 当用户明确要求桌面或系统截图，或工具自带截图能力不可用而需要 OS 级截图时使用。支持全屏、特定应用或窗口、像素区域等场景。 | `docs/reference/skills/openai/screenshot/SKILL.md` |
| `openai/security-best-practices` | `security-best-practices` | 执行特定语言和框架的安全最佳实践审查并提出改进建议。仅在用户明确请求安全最佳实践、安全审查报告或安全默认编码帮助时触发；支持 Python、JavaScript/TypeScript 和 Go。 | `docs/reference/skills/openai/security-best-practices/SKILL.md` |
| `openai/security-ownership-map` | `security-ownership-map` | 基于 Git 仓库历史构建安全所有权拓扑，计算 bus factor 和敏感代码 ownership，并导出 CSV/JSON 用于图数据库和可视化。仅在用户明确需要安全导向的 ownership 或 bus-factor 分析时使用。 | `docs/reference/skills/openai/security-ownership-map/SKILL.md` |
| `openai/security-threat-model` | `security-threat-model` | 基于仓库生成威胁建模，枚举信任边界、资产、攻击者能力、滥用途径和缓解措施，并输出简洁 Markdown 威胁模型。仅在用户明确要求对代码库或路径做 threat model、列举威胁或进行 AppSec 建模时使用。 | `docs/reference/skills/openai/security-threat-model/SKILL.md` |
| `openai/sentry` | `sentry` | 当用户要求查看 Sentry issue 或 event、总结近期生产错误，或通过 Sentry CLI 拉取基础健康数据时使用；使用 `sentry` 命令执行只读查询。 | `docs/reference/skills/openai/sentry/SKILL.md` |
| `openai/skill-creator` | `skill-creator` | 创建有效 Codex skills 的指南。适用于用户希望创建新 skill，或更新已有 skill，以扩展 Codex 在专业知识、工作流或工具集成方面的能力。 | `docs/reference/skills/openai/skill-creator/SKILL.md` |
| `openai/skill-installer` | `skill-installer` | 从 curated 列表或 GitHub repo path 安装 Codex skills 到 `$CODEX_HOME/skills`。适用于列出可安装技能、安装 curated skill，或从其他仓库安装 skill，包括私有仓库。 | `docs/reference/skills/openai/skill-installer/SKILL.md` |
| `openai/speech` | `speech` | 当用户要求文本转语音旁白、voiceover、无障碍朗读、音频提示，或通过 OpenAI Audio API 批量生成语音时使用。使用内置 CLI 和内置 voices，实时调用需要 `OPENAI_API_KEY`；不包含自定义声音创建。 | `docs/reference/skills/openai/speech/SKILL.md` |
| `openai/transcribe` | `transcribe` | 将音频文件转录为文本，可选说话人分离和已知说话人提示。适用于转录音频或视频中的语音、从录音提取文本，或标注访谈和会议中的说话人。 | `docs/reference/skills/openai/transcribe/SKILL.md` |
| `openai/vercel-deploy` | `vercel-deploy` | 将应用和网站部署到 Vercel。适用于用户请求部署应用、获取部署链接、上线项目或创建 preview deployment。 | `docs/reference/skills/openai/vercel-deploy/SKILL.md` |
| `openai/winui-app` | `winui-app` | 使用 C#、Windows App SDK、官方 Microsoft 指南、WinUI Gallery 模式、Windows App SDK 示例和 CommunityToolkit 组件，启动、开发和设计现代 WinUI 3 桌面应用。适用于创建新应用、准备环境、审查、重构、规划、排错、主题、可访问性、响应式、性能和部署。 | `docs/reference/skills/openai/winui-app/SKILL.md` |
| `openai/yeet` | `yeet` | 仅当用户明确要求在一个流程中 stage、commit、push 并使用 GitHub CLI (`gh`) 打开 GitHub Pull Request 时使用。 | `docs/reference/skills/openai/yeet/SKILL.md` |
