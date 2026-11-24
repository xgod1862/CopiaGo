use super::{Package, PinnedFile, Source};

pub const DDI_REPO: &str = "doronz88/DeveloperDiskImage";
pub const DDI_REF: &str = "dcec53989fa57695c2b3d3e11c2585f4735b2fef";
pub const DDI_BUILD: &str = "27B52";

pub(super) const SOURCES: [Source<'static>; 3] = [
    Source {
        name: "raw.githubusercontent.com",
        url: "https://raw.githubusercontent.com/{repo}/{ref}/{path}",
    },
    Source {
        name: "ghfast.top",
        url: "https://ghfast.top/https://raw.githubusercontent.com/{repo}/{ref}/{path}",
    },
    Source {
        name: "fastly.jsdelivr.net",
        url: "https://fastly.jsdelivr.net/gh/{repo}@{ref}/{path}",
    },
];

// Fixed sizes and SHA-256 from the Python 1.2.0 developer_setup.py.
pub(super) const PERSONALIZED: Package = Package {
    folder: "Xcode_iOS_DDI_Personalized",
    files: &[
        PinnedFile {
            local: "BuildManifest.plist",
            remote: "BuildManifest.plist",
            bytes: 800656,
            sha256: "3c4eee2be4a0a853044dc32dbbd62699302b1dfd317aaf4f1cf36368b7639bb0",
        },
        PinnedFile {
            local: "Image.dmg",
            remote: "Image.dmg",
            bytes: 17084416,
            sha256: "0de55b0ccde9d5790c9c9f4e1a413225443fce7ea75acb503aca11e7d8156646",
        },
        PinnedFile {
            local: "Image.trustcache",
            remote: "Image.dmg.trustcache",
            bytes: 1851,
            sha256: "f0fb56da2829f69b8aae113584349635738e68d77c82a726ed72040890f16bf4",
        },
    ],
};

pub(super) const CRYPTEX: Package = Package {
    folder: "Xcode_iOS_DDI_Cryptex",
    files: &[
        PinnedFile {
            local: "BuildManifest.plist",
            remote: "BuildManifest.plist",
            bytes: 804097,
            sha256: "537391eb2f37e604fc693f422c1568af97220067853e663391ba3e8258569d6e",
        },
        PinnedFile {
            local: "Image.dmg",
            remote: "Image.dmg",
            bytes: 17239040,
            sha256: "500d4139f7963650e6c796d950267ed38909e7d183ad26bba073257529bfe173",
        },
        PinnedFile {
            local: "Image.dmg.trustcache",
            remote: "Image.dmg.trustcache",
            bytes: 1851,
            sha256: "2719aa3f1c99245432512116fb8a56d55ec224c706e545772d4c17c1233ac475",
        },
        PinnedFile {
            local: "Image.dmg.cryptex_info",
            remote: "Image.dmg.cryptex_info",
            bytes: 428,
            sha256: "0180ee20ce5daa7978fe38f24c742361726789ea6af82c3c1d2afc480734a531",
        },
        PinnedFile {
            local: "Image.dmg.root_hash",
            remote: "Image.dmg.root_hash",
            bytes: 229,
            sha256: "5f3281961396be5fe008e11641ffc25c7db24c11d3789ff0c4ff0fa6d47b94ca",
        },
    ],
};
