import { Daytona } from "@daytona/sdk";
import type { Sandbox } from "@daytona/sdk";
import fs from "node:fs";

const daytona = new Daytona();

// sandbox 名字
const SANDBOX_NAME = "Perihelion Sandbox";
// 仓库要下载的地址
const MOUNT_DIR = "/home/daytona/code";
// github 仓库拉取数据的地址
const GIT_URL = "https://github.com/KonghaYao/peri.git";
// config 的地址
const DEFAULT_PERI_CONFIG_PATH = "./settings.json";

/** ExecuteResponse 的本地子集 — @daytona/sdk 未公开导出此类型 */
interface CommandResult {
    exitCode: number;
    result: string;
}

type PeriConfig = Record<string, unknown>;

// peri 的默认配置，写入 sandbox 内的 ~/.peri/settings.json
const periConfig: PeriConfig = JSON.parse(
    fs.readFileSync(DEFAULT_PERI_CONFIG_PATH, "utf-8"),
);

/**
 * Shell-escape 一个值，安全地嵌入命令字符串。
 * 用单引号包裹，转义内部单引号。
 */
function shellEscape(value: string): string {
    return "'" + value.replace(/'/g, "'\\''") + "'";
}

/**
 * 在 sandbox 内按顺序执行一组 shell 命令。
 *
 * @param sandbox - 已获取的 sandbox 实例
 * @param commands - 要执行的命令列表，按顺序依次执行
 * @param cwd - 命令的工作目录（绝对路径）
 * @returns 每个命令的执行结果数组
 * @throws 任意命令 exit code 非 0 时立即抛出异常，后续命令不再执行
 */
async function executeCommandList(
    sandbox: Sandbox,
    commands: string[],
    cwd: string,
): Promise<CommandResult[]> {
    if (commands.length === 0) {
        return [];
    }
    const results: CommandResult[] = [];
    for (const command of commands) {
        const response = await sandbox.process.executeCommand(
            command,
            cwd,
            undefined,
            120,
        );
        console.log(
            `Command: ${command}\nExit Code: ${response.exitCode}\nStdout: ${response.result}`,
        );
        if (response.exitCode !== 0) {
            throw new Error(
                `Command failed with exit code ${response.exitCode}: ${command}\n${response.result}`,
            );
        }
        results.push(response);
    }
    return results;
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
/**
 * 初始化 sandbox 并将数据写入 volume。
 * 完整流程：
 *   1. 获取/创建持久化 volume（代码跨 sandbox 保留）
 *   2. 创建新 sandbox 并挂载 volume 到 MOUNT_DIR
 *   3. Clone peri 仓库到挂载目录
 *   4. 安装 peri CLI 并写入默认配置
 *
 * 此函数由 PUT 请求触发，用于首次部署或重新初始化环境。
 *
 * @param gitUrl - peri 仓库的 clone 地址
 * @param config - 要写入 .peri/settings.json 的配置对象
 */
async function initSandbox(gitUrl: string, config: PeriConfig): Promise<void> {
    console.log("[initSandbox] Step 1/4: Getting or creating volume...");
    // const volume = await initVolume();
    // console.log(`[initSandbox] Volume ready: ${volume.id}`);
    await sleep(10000); // 等待 volume 状态稳定

    console.log("[initSandbox] Step 2/4: Creating sandbox...");
    const sandbox = await daytona.create({
        name: SANDBOX_NAME,
        language: "typescript",
        // volumes: [
        //     {
        //         volumeId: volume.id,
        //         mountPath: MOUNT_DIR,
        //     },
        // ],
    });
    console.log(`[initSandbox] Sandbox created: ${sandbox.id}`);

    // Clone repo 到挂载的 volume
    console.log(
        `[initSandbox] Step 3/4: Cloning ${gitUrl} into ${MOUNT_DIR}...`,
    );
    await sandbox.git.clone(gitUrl, MOUNT_DIR, "main");
    console.log("[initSandbox] Clone complete");

    // 安装 peri CLI（install.sh 内部会 clone，无需重复 clone）
    console.log(
        "[initSandbox] Step 4/4: Installing peri CLI and writing config...",
    );
    await executeCommandList(
        sandbox,
        [
            "curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.sh | bash",
            `mkdir -p ${MOUNT_DIR}/.peri && cat <<'EOF' > ${MOUNT_DIR}/.peri/settings.json\n${JSON.stringify(config, null, 2)}\nEOF`,
        ],
        MOUNT_DIR,
    );
    console.log("[initSandbox] Done — sandbox initialized successfully");
}

/**
 * 向 peri AI Agent 发送单轮问答请求（print 模式）。
 *
 * 通过 `peri -p` 非交互模式执行，返回模型输出的纯文本。
 * sandbox 按 SANDBOX_NAME 查找——必须已由 PUT 初始化。
 *
 * @param inputPrompt - 用户输入的自然语言提示
 * @returns peri 的非交互输出文本
 * @throws sandbox 不存在或命令执行失败时抛出异常
 */
async function askPeri(inputPrompt: string): Promise<string> {
    const sandbox = await daytona.get(SANDBOX_NAME);
    const results = await executeCommandList(
        sandbox,
        [`/home/daytona/.peri/peri -p ${shellEscape(inputPrompt)}`],
        MOUNT_DIR,
    );
    return results[0]!.result;
}

/**
 * HTTP 请求路由分发器。
 *
 * POST /  → 向 peri 发送问答请求，body: { prompt: string }
 * PUT  /  → 初始化/重建 sandbox 环境（幂等，不依赖已有 sandbox）
 * GET  /  → 健康检查，返回 "Hello, World!"
 *
 * 其他 HTTP 方法由 Daytona 平台层处理（如 sandbox 生命周期管理）。
 */
async function handle(request: Request): Promise<Response> {
    // --- POST: 执行 peri 问答 ---
    if (request.method === "POST") {
        let body: unknown;
        try {
            body = await request.json();
        } catch {
            return new Response("Invalid JSON body", { status: 400 });
        }
        const { prompt } = body as { prompt?: unknown };
        if (typeof prompt !== "string" || prompt.length === 0) {
            return new Response("Missing or invalid 'prompt' field", {
                status: 400,
            });
        }
        try {
            const result = await askPeri(prompt);
            return new Response(result);
        } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            return new Response(`Error: ${message}`, { status: 500 });
        }
    }
    // --- PUT: 初始化 sandbox 环境（首次部署或重建）---
    if (request.method === "PUT") {
        try {
            await initSandbox(GIT_URL, periConfig);
            return new Response("Sandbox initialized successfully.");
        } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            return new Response(`Initialization failed: ${message}`, {
                status: 500,
            });
        }
    }
    // --- GET / 其他方法: 健康检查 ---
    return new Response("Hello, World!");
}

export default {
    fetch: handle,
    idleTimeout: 120, // 专门给 bun 的
};
