import { getReleases, formatReleaseInfo, findAssetForPlatform } from "../utils/github.js";
import { getCurrentVersion, getPlatformInfo } from "../utils/config.js";

export async function listVersions() {
    const platformInfo = getPlatformInfo();
    const currentVersion = await getCurrentVersion();

    console.log("📋 Available versions:");
    console.log("");

    try {
        const releases = await getReleases({ perPage: 5 });

        if (releases.length === 0) {
            console.log("No releases found.");
            return;
        }

        const formattedReleases = releases.map(formatReleaseInfo);

        formattedReleases.forEach((release, index) => {
            const isCurrent =
                release.tag === currentVersion ? " (current)" : "";
            const isLatest = index === 0 ? " (latest)" : "";

            console.log(
                `  ${index + 1}. ${release.tag}${isLatest}${isCurrent}`,
            );
            console.log(`     Name: ${release.name}`);
            console.log(`     Published: ${release.publishedAt}`);
            console.log(`     URL: ${release.url}`);

            // 显示匹配当前平台的二进制文件
            const platformBinary = findAssetForPlatform(
                { tag_name: release.tag, assets: release.assets },
                platformInfo,
            );

            if (platformBinary) {
                console.log(
                    `     Binary: ${platformBinary.name} (${platformBinary.size})`,
                );
            } else {
                console.log(
                    `     Binary: Not available for ${platformInfo.target}`,
                );
            }

            console.log("");
        });
    } catch (error) {
        console.error("❌ Failed to fetch releases:", error.message);
        process.exit(1);
    }
}
