import { getLatestRelease, getLatestReleaseByPrefix } from "../utils/github.js";
import { getCurrentVersion } from "../utils/config.js";
import { install } from "./install.js";

export async function update(pkg) {
    try {
        const prefix = pkg ? `${pkg}-` : "agent-";
        const currentVersion = await getCurrentVersion();

        console.log("🔍 Checking for updates...");

        const latestRelease = await getLatestReleaseByPrefix(prefix);

        if (currentVersion && latestRelease.tag_name === currentVersion) {
            console.log("✅ You are already on the latest version!");
            console.log(`   Current: ${currentVersion}`);
            return;
        }

        if (currentVersion) {
            console.log(`🔍 Current version: ${currentVersion}`);
        }

        console.log("");
        console.log(`🆕 New version available: ${latestRelease.tag_name}`);
        if (currentVersion) {
            console.log(`   Current: ${currentVersion}`);
        }
        console.log("");

        // 安装最新版本
        console.log("🚀 Starting update...");
        await install(latestRelease.tag_name);
    } catch (error) {
        console.error("❌ Update failed:", error.message);
        if (error.stack) {
            console.error(error.stack);
        }
        process.exit(1);
    }
}
