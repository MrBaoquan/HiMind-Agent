# 微信小程序体验版交付

通用微信小程序体验版交付工作流。构建指定工程，冻结候选，校验上传环境，审批后上传体验版。

## 参数

- `workspace_root`：Git 工作区目录。
- `project_root`：包含 `project.config.json` 的小程序工程目录。
- `app_id`：目标小程序 AppID。
- `version`：体验版版本号。
- `description`：版本说明。
- `channel`：`auto`、`ci` 或 `wechatide`。
- `package_manager`：`npm`、`pnpm` 或 `yarn`。
- `script`：构建脚本名，默认 `build`。
- `artifact_dir`：构建产物目录，留空使用项目内默认目录。
- `credential_handles.private_key_path`：CI 上传私钥句柄。

## 上传通道

- `ci`：使用 `miniprogram-ci` 和上传私钥，全程无人值守。
- `wechatide`：使用微信开发者工具，需要用户在窗口内确认；pending 任务不会重复发起。
- `auto`：有私钥走 CI，无私钥走开发者工具。

## 步骤

1. `WX-BUILD`：执行项目构建脚本。
2. `WX-CANDIDATE`：冻结不可变 Git 候选。
3. `WX-READINESS`：检查项目配置、Node、私钥、CI 与开发者工具通道。
4. `WX-UPLOAD`：审批后上传体验版并记录 Artifact。

## 边界

- 不自动提交微信审核。
- 不伪造平台状态。
- 不把 pending 任务写成上传成功。
- 不读取或输出私钥内容。

