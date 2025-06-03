use std::fs;
use std::path::{Path, PathBuf};
use std::io::Read;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use base32;
use walkdir::WalkDir;
use sha2;
use sha2::Digest;
use color_eyre::{Result, eyre::eyre, eyre::WrapErr};

// Step 1: Compute SHA256 hash  Output: 32-byte bytes object.
// Step 2: Compress(XOR) hash   Output: 20-byte bytearray.
// Step 3: Encode base32        Output: 32-character string.
// Step 4: Convert lowercase    Output: 32-character lowercase string.
pub fn b32_hash(content: &str) -> String {
    // Step 1: Compute hash
    // - The Sha256::new() function initializes a SHA256 hasher.
    // - The .update() method feeds the input string (as bytes) into the hasher.
    // - The .finalize() method computes the hash and returns it as a 32-byte array.
    let mut hasher = sha2::Sha256::new();
    hasher.update(content);
    let sha256_hash = hasher.finalize();

    // Step 2: Compress hash
    // - A [u8; 20] array is created to store the compressed hash.
    // - Each byte of the first 20 bytes of the SHA256 hash is XORed with the corresponding
    //   byte of the next 12 bytes. This ensures all bits contribute to the final hash.
    // - The result is a 20-byte compressed hash.
    let mut compressed_hash = [0u8; 20];
    for i in 0..20 {
        compressed_hash[i] = sha256_hash[i] ^ sha256_hash[i + 12];
    }

    // Step 3: Encode base32
    // - The base32::encode() function encodes the compressed hash into a base32 string.
    // - The Alphabet::Rfc4648 { padding: false } ensures no padding is added.
    // - The result is a 32-character string.
    let b32sum = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &compressed_hash);

    // Step 4: Convert lowercase
    // - The base32 string is converted to lowercase using .to_lowercase().
    // - This ensures consistency in the output, as base32 encoding can produce uppercase
    //   letters by default.
    // - The result is a 32-character lowercase string.
    b32sum.to_lowercase()
}

pub fn epkg_store_hash(epkg_path: &str) -> Result<String> {
    let dir = Path::new(&epkg_path);

    let fs_path = dir.join("fs");
    let install_path = dir.join("info").join("install");

    // 收集所有文件和目录的路径
    let mut paths: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| entry.ok()) // Skip errors
        .map(|entry| entry.into_path())
        .filter(|entry| entry.starts_with(&fs_path) || entry.starts_with(&install_path))
        .collect();

    paths.sort();

    let mut info: Vec<String> = Vec::new();

    for path in &paths {
        // if path == dir { continue; } // this is where rust WalkDir differs from python os.walk
        let (ftype, fdata) = get_path_info(&path)
            .wrap_err_with(|| format!("Failed to get path info for: {}", path.display()))?;
        info.push(path.strip_prefix(dir)
            .wrap_err_with(|| format!("Failed to strip prefix '{}' from path '{}'", dir.display(), path.display()))?
            .to_string_lossy().into_owned());
        info.push(ftype.to_string());
        info.push(fdata);
    }

    let all_info = info.join("\n");
    // println!("{}", all_info);

    Ok(b32_hash(&all_info))
}

fn get_path_info(path: &Path) -> Result<(&str, String)> {
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("Failed to get metadata for: {}", path.display()))?;

    let (ftype, fdata) = match metadata.file_type() {
        ft if ft.is_symlink()       => ("S_IFLNK", fs::read_link(path)
                                        .wrap_err_with(|| format!("Failed to read symlink: {}", path.display()))?
                                        .to_string_lossy().into_owned()),
        ft if ft.is_file()          => ("S_IFREG", file_sha256_chunks(path, &metadata)?.join(" ")),
        ft if ft.is_block_device()  => ("S_IFBLK", metadata.dev().to_string()),  // u64
        ft if ft.is_char_device()   => ("S_IFCHR", metadata.dev().to_string()),  // high32-major  low32-minor
        ft if ft.is_dir()           => ("S_IFDIR", "".to_string()),
        ft if ft.is_fifo()          => ("S_IFIFO", "".to_string()),
        ft if ft.is_socket()        => ("S_IFSOCK", "".to_string()),
        _ => return Err(eyre!("Encountered an unknown file type at: {}", path.display())),
    };

    Ok((ftype, fdata))
}

/// Compute the SHA-256 hash for every 16 KB chunk of a file.
/// One-shot computation could consume too much memory for large files.
fn file_sha256_chunks(file_path: &Path, metadata: &fs::Metadata) -> Result<Vec<String>> {
    const CHUNK_SIZE: usize = 16<<10; // 16 KB

    let mut file = fs::File::open(file_path)
        .wrap_err_with(|| format!("Failed to open file: {}", file_path.display()))?;
    let mut buffer = vec![0; CHUNK_SIZE];
    let mut hashes = Vec::new();

    hashes.push(metadata.len().to_string());

    loop {
        let bytes_read = file.read(&mut buffer)
            .wrap_err_with(|| format!("Failed to read from file: {}", file_path.display()))?;
        if bytes_read == 0 {
            break; // End of file
        }

        // Compute the SHA-256 hash of the chunk
        let mut hasher = sha2::Sha256::new();
        hasher.update(&buffer[..bytes_read]);
        let hash = format!("{:x}", hasher.finalize());
        hashes.push(hash);
    }

    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_b32_hash_for_which() {
        let content = r#"fs
S_IFDIR

fs/etc
S_IFDIR

fs/etc/ima
S_IFDIR

fs/etc/ima/digest_lists
S_IFDIR

fs/etc/ima/digest_lists/0-metadata_list-compact-which-2.21-16.oe2403.x86_64
S_IFREG
2619 e7b29a08f632e9e67c6d46515118a94febc4de8872a7b0ecfadc176fff04892a
fs/etc/ima/digest_lists.tlv
S_IFDIR

fs/etc/ima/digest_lists.tlv/0-metadata_list-compact_tlv-which-2.21-16.oe2403.x86_64
S_IFREG
192 b544f3a4a6e27f165f60c58f5bcd3b973eeb40f13b0e8b4866f10115c3ee602b
fs/usr
S_IFDIR

fs/usr/bin
S_IFDIR

fs/usr/bin/which
S_IFREG
31512 b1bdc41118c68971fb5c19ed56719d737fd48ac0f572c3c7ae25d7d9987c399d 2874436af593f27aac22a4f002768aeeb689f55e0fc1e7341df7a949baf25482
fs/usr/share
S_IFDIR

fs/usr/share/licenses
S_IFDIR

fs/usr/share/licenses/which
S_IFDIR

fs/usr/share/licenses/which/AUTHORS
S_IFREG
207 b43569e54c311b794fe65f87e3142448333e15954fa0ce522d8ccad4c172ac0b
fs/usr/share/licenses/which/COPYING
S_IFREG
35147 4640533f6a2164475200c008c0dd97eef50a7020fb924d00563f3bd2c8400a1a bca4e8f27aaa00867178fe0adc7e4559d0396118346e3143d94479025241ab6a 683acd90103a7b7e9f859f9876a9e0536701bd56d09dba6672b0c9fce4b8fe40"#;

        assert_eq!(b32_hash(content), "pm7cnxioa7iycyeyabryerpiyx26lgwm");
    }

    #[test]
    fn test_b32_hash_for_zstd() {
        let content = r#"fs
S_IFDIR

fs/etc
S_IFDIR

fs/etc/ima
S_IFDIR

fs/etc/ima/digest_lists
S_IFDIR

fs/etc/ima/digest_lists/0-metadata_list-compact-zstd-1.5.5-1.oe2403.x86_64
S_IFREG
2747 671389c9d1a0a16f3c73e59f6956fa40b48203f01487f554ee4e97bbd29e9c94
fs/etc/ima/digest_lists.tlv
S_IFDIR

fs/etc/ima/digest_lists.tlv/0-metadata_list-compact_tlv-zstd-1.5.5-1.oe2403.x86_64
S_IFREG
556 964691125fcd041fe6fe6afcef11c018be9a5c9cc6b2a3c2ccb6578ad0997c95
fs/usr
S_IFDIR

fs/usr/bin
S_IFDIR

fs/usr/bin/pzstd
S_IFREG
776544 c15d27dfe183b2a42c3b5ced09c3162e38e8164de16448c35bab8b02849b104c b5a7c776c4964aa71bc31916dc22f90e3a9e9310259fc7bd77a662dc06175ad0 bbf4b80db2750240da5204d83f55e98f0246a537b452c8e0e1242659c479b814 21eaa422f52537d53a553487e243242722aa4efd57456f8c3c19f487a44ef0f7 eefaf0f1f99fe36c99e13ca4eeffd188d0f5373366a2b72602392cf9f3093acc 9fd94eb0053e3939730d1fd9257ead71ae8a30a25f6f547af99e170210268128 f68ae9526b62f249a6e05d67465f63ac16f887bca476e8c3e03de3878c07a8dc 4ef63c7eb26d8634949e635efbd163ff55c0010fdc0ed314fb693d11db064d85 4b31058d920dd0ec93fafb1d98b454236d17938e7a3bfe15205647b2c25063c9 c7049b93fb285cea744f84d5e96bb4cc93ea4d6c24cf0424b85ceb31d7abbcab 4849ea709214d80ef45e0fe12774b269377d7dd8c93ba465b3f6ed820726ea1e 2cc7c577e5e3d1ab75efff1def52a082e557093527cb58be454b22cca1f9b8ea f0b3a2af86aab9291f101085f1d7232c6f9cb2b84e6594ca160c0916b1d3eaba 1a0d59f23614c12b0b061aa984b57eca279fd065162f5e7218b01128d69ddf3d f2bdeb06d906e818cd5cf3c1b053d737191aaa3801d80739bf87f9f1b9b8afe2 21207a2a94282280da47136637b644c554cb71b8d796020d5be3619d8374c6a8 3e5aa48fc7dd771fbb8e616a053a904b4cfc27dba281b8e43501c09aa88048f9 12dbc4d063ba9e774a9fd02a1151a1beaf4369d217e450e9ddf28aae61c6266b b954bba49df5041d89e72b7ea4f6d2c0a4e67714a9267ca69249d94e7fe5c0e1 f42c9d7d93a152dbbc1830a12b555239b138ba515c095fa051c7561628d010ca 0333df6365ca0c4f7af099d21c371e600012f640045a61558d01120e53d89d6e 77fca085bc06dec0bfbc5dc0f45e21c3fa1304f5c207f422a4849214f42acbe1 f283c1fe68594cf881159832a01e4b01e98ddfbd0d50fe93ae8a9410240b4dfd 62eacfe40596c28504cac1f10a8f7c952441bf066bbb8bfb61f1f8e06f1d2574 8bba0397b2c119d270731ae79be9363999c7dc8dc033ac8129e4a76774f1e091 ba05e9d54310ba1d2fd9bc7f73c46ac4e2f2bf463f79f922de91a4edf84d42b2 3ffcdd3cb3b95014b557d5c0ac6ee0fa530d7b9fe5e3f237fe85112bb52bac1b 05c817c2c85c2bfdf433aa1c7fba6189cc7ab9f9ae4cc0e6acbab15dde0f8951 5f2189fba92cd595e0b24bd651524045b3c79932d6d9b67afcd05b8b63caf900 c6a76a4c41bcd47d272ed704e090b585142f3f000bae8deae1d346acaebd264b 0b15e8114433827baeeec48c1f89bf6d56b2bb4efe92b6a69cd79f2e0aeffd44 cd251a600c35debdc49a1525657e2d8de736a56b2e8712039512ae801869051b eb907bef293f8d2c67580b255117987eaab79dfd1aa18f5c256e4e568e22d4ae f8faffe4b16017d612dd9700aed55a3ae6d9b77cec13d3758a5f33f10c56c2b7 23e2fff27f20f2516ad5749d8a99e29a04dc7d01fbfb96201fb98674568982d1 c17260c7697d361febea75a8eb0a59d049c75eaaebcd73ccf56c636e91c58072 585227544fbc5480687eef48fd8da83c6b4e1707ef3224cc069bb463cb8c5831 b710250e129180fdfee4b53cb8712807c30a3dd19b3e44c1517b9a1da70f12a1 52ffba761f05ca756a4c211e6f7397d45c1c3255473bc6c1cf6596cd35a00e6e b62922dbb6d17aa208898d5cb217408f8c3a2c891a06ea1b4472c49fe1043493 6bfbfa59fb09270bfa3ea8c8762784d9d43d118451c63eeb052920749b29bc74 8669e0ebddf663ee55f4b6bfe2c340ef61472b02c632ce2f32cc57acefd08288 409b6a5123fbbd4009832d7382bd739eabe0b5fdf97d826e07d9250012f01475 9657ed81cfd0ae471e2b305897c2df6df0708143f19cfc01cd344dc4b99f26fa 3bd7bb9ff2b6593af23ab00398c3d44d3b86f65247c247ca76ef8316a9351a45 e1b065c8240131fd52d4c2e74f5488d2f0aadffc53c1080ce56a1e5d6fe6fe56 7900ff8c5f2a36a722fc00b59d3c21efb1fed361c93c2c32baf186eba8bc56b3 2064714fdab042e52152bb43c9f8707e48f1aeccf2772d8a318be1f42a5065dd
fs/usr/bin/unzstd
S_IFLNK
zstd
fs/usr/bin/zstd
S_IFREG
928280 285d5d5f7469aa1b21f87ae41c4bf77443bf3ed2ac0c4bf6af2a0860b8af6cbc 98183e91f994238c129fbfcacf8a5e955b8b6d4cca4cf0a346e41124e9eb5a10 f92d93fbc66734ac6e1ffaa575ac92b27acda37c0e4ac30cb4409eaafd25eb20 f50983b89d039c913c23665fb18ae1c8730bf251238ac425d497f2a18091dcca 5f44625ef016ee06941b92b00e2b4f862268fa38b89f30e0a6cc651d74a7b7fb a50db3aa2eb4eee22c4d98e145ac5fced0f7ff88d55a8a8ce8420fbed3e49f8b b53b39ea0b104e0146656283012b0e556a8e226edd67ae71dce332706b5030a6 8d3b5dee72872d405156901d0cb9587ed8859a1e1cd34801e86a1f2503284c4d 6641ca010c5bf6330374e18a44c3cbcc186b0d94e231b11b1c7eb8a9c859104f f7be0fa09167126d6a4bbbfff5ebdc13bf60659996df78d673fff331fc3bc20b 00b16d6a6efc44dbfa7df94d861d516f470bd2504d03d58284329ec4cbc2209d 94e00e48194a8be89aafa37ecc7b0b1305142f85f24bd1e25ea312e577c2f391 b2c8433c1c29f3709054908d400493512a94c33edd2be70848026ce48f0bffc4 447a464a32a0cad9009e8d072be905aee59b220eb735de238d2c7053708668ba 95db29a75b3e02c483da6d60edb7f3738348ea7998e06669adc2eff773ca4510 a9ecadd729b9f0c6f6b8e06cd5c6273fabf66e7b21aaf019fdb704cae5474f51 fb686e8b58a8ce33f6364f980cc754a9ff923655ce79ebcfadee4947f2aadf6d 565de86dbb6d18d50c3282aa604a4c16070250ed36e6d59c594d084648d1c362 174f875bb3419e2e9fbb8c1a82ac35a4fa182e94bd8a373804d5ea6f37970e1f d8cbbe9b0480ab6660a79dacedfb305607888679f0b816828b6fd8613253df5c 3464d55a2b4a2584c6abfc5602f245f635a9454f0d5fdddcbb8686327cbf63eb a41ea283e369c4ec2944b9ed7729f6fc8cdb4126279eaa87c9da46a003316c29 ea50f0f69a3d35f2dbf9789cde425cbc985fcb3b8b530b40930c5ce2621c7448 d1c637c5ff7181ac42bf55ef5c86bc9b56c9f19137aec973c8994cf6767e1c20 5974057b8a3886f4f5cfdd0702770f0994175207ce2c04587200aada54fe42d4 ee9583897434484dea27daabf19ebf1fc90f0177362ff3b9aed45ac15606a9d4 d63e95dfc3e4a630e7d3f9ec663d0de1803ec7da69c82e1e8e8194baa226850b 7d4caf91feb92569746ef9e08b92b410b36907be2c42decaa5262b0a8451c35e 3394249549c39229a6f96cbe050d169b71873e5f24895e852aa44c268b856851 36bbcc008821050b394b52417b5dc2d0975fe7bb746e9c51bdfc5370df1d3135 3f1a80e0a36e30bae61f3d37d25f2244d632ccc588aeb9b28653ed4db9f413e0 64ce4cec66fe2344de6c2460b5a325c8e07a7cd7bf426a52b4a0d888988bbc76 79b2efa86b206b7268e4f0a55bec524669e00480762607335244f28411ffd530 e3202798084eb38cb8b75579afcd03f6e2c8d59d865b7b0192407366ed6056d9 6bbdaa7e62d133aaa482abdfb135632f28bb4ea45b60f09e046b262b4880d6df e4e97168f2f667dc21e7ffc156c7a0ac8fea06c1bf00dadf21fd2b70f49df307 8129d77e742f74ae57d52df6b35d83e2b5f3fdfbb9a336f67d6b4a89283aa7f4 0e8f51a05d1f8b7d94b91117d5f054b5084eb0e6eba38c6ea36394fa52892744 26b2b65e6aa4557e9c5f6ba5fb3872aec0025dc2987bdc3dabf00cbc7002b0fd c84f9af6f4736039cc290dba2dddda67d68a809010e33747419864f15c4b8955 2347bd7dfbdb4246656550ff2b4e184f8783aeccae4dc88d3115b1b672bc343e 51f0b176ae3c80550216ec105e00e9d19a936a428d58cc271ce774c98b893dfe 4da30f2e84edf697eb8b1105cbb5a44c60e86e9f0086b6fbedec3305a68fb2c4 5a019d70bbd058bb1247ed19d78ab00334a30b7855832bebeeb676c543360102 29e7709f2647bb74e6c150701309d4d5f9913b92db2dba384dc9ebf4d53a91d3 3c19ed48bb7be3c258a1bdf9557051270bb53446b77f96565b7b11bb35dd3b84 e8e1687067dd1ceed64ca3a175b987c50b7a0614afe28fb978112cf658bb6202 8d85cf9fbb029c6ca54c19991d0e3c83c3fa581a220d43ffaf874dde4e3723a5 99211babb5186b33eb68eff0ce275f90ee3538a93268a0ae650145caadb8edd6 d1f19274721b2255acf38d06e46a92d6bae05a131d7ca4259dc77ee1f77552b6 34582669e5f89e9c49751465f64798862e7a6d3b1bd7453da16c8e6aa5189958 548dcc1efa4db8914d16ee57245192bff2e0da68e00575281faf99caf2582f60 d4ebd8abb11e5b1d886f44dd00d49959c94dd73e6d6e972d7d62daa193cddcf2 5c1ef5f73b381e38aa491494789d39d87d6d75287571664a525ec8f40118405c 01fe3dffab043444b9a92387c91f735fef3d4c2549f94613193caa48491d0211 f3eb12a9d65b61513bbf308c40935fbe8b9d6857ed8fcb89116a292f487d659a e2adff028411f2211f1458765103a9c6db6919ed7cb87072065c45c6695a92aa
fs/usr/bin/zstdcat
S_IFLNK
zstd
fs/usr/bin/zstdmt
S_IFLNK
zstd
fs/usr/lib64
S_IFDIR

fs/usr/lib64/libzstd.so.1
S_IFLNK
libzstd.so.1.5.5
fs/usr/lib64/libzstd.so.1.5.5
S_IFREG
981112 ee57c8a24992d97d70fa0c5dda571d1fe6bcc5e8a066facee231a5348f88b76e 2124cbd407f6a77fe96ac03385694106aaff58a5142bce05da139bf7a7d7e1f9 96456842fe78b11b81240b323602c2c9481e1bc1bf9d4c0059097aaa230d7590 268c23dbb74eb16f686979e64b794de52c58a36b0c5a2436e600900461de4d3b 8e6333d26f0b5d9d69cbd4f3d19f76ef3181fdb317b17123dae33ba144bd4f93 526c8f406486b9845ca0970c5e9072a5cc82d7653e119cc263b1201a754416e7 7103550601a0f43e09216db31a454a4dd0f077965defe2f579adaba8e7643895 b4baa968128cde01393290d6d0476ba20768ce94657e4b14405af1df4ddd2dac b63eec9fd237bae23086b893e443d5b2ae09721ace894ec2123817c1d3272886 be44c62ade4491aee6f43efdf729ee8eba1964fcb38d80279d9648b993dc11bb 340ea69ca53e7df46ec745fa43ebe1d224bf38d93a37f1665539a7c782e0c1b0 4759f379cce20b69a62467bad0eb503daf1bdd2fcfb270be8026e2470dc172f4 34c36c44c50a83b85f804f7db727f02f85d5b00bb4066e09388609d830140330 14c01d72eaaa350faefad7c679d15b9d6f776d30a4316a03bd0449fee7b0fd8f be9628a2a64d7caaec9352c3d40c1d6b118e82233c22def8c1df7e2ab33d559d e3c2b74c52934cbcba019dae5fba3454b0881a592f736e05290412d242170199 09ca9314fe02ec8418db92b82eae55a3ca8d68207697cc187a8c813223d96825 9c4a95e56c09c6d3dee388cf639e5a5eb2e783b43e2669341c61163993bd9605 d7d719d8923ebcacc54cf19282644465b860f071f2cbdeb8acf7a9f1450df413 3fa8a0cca69a82c2b15e2d210f85eb78a8f379b4acb63dc7d443ba1cb34ccdb3 0ca29dd92ba00611af9407e3cf31f9d424ce7a16eefc8dd5460455219fd653b2 942fbe48e26b719a8e95e7ba872e31b6d2df0908c6f43af24d6cc886ce3982c9 8a8a280e6cbfcccc074513a81333bf2ee91ac37019a091874289a5920f74b890 4e183b0c7446b2fcae273d16051e281e3a4bf3c491244328f078d0204f20d985 d208b33a581f546eb056792c756bebb3eeac01634194a92091738c3413af7012 418b849437a690472a099dc5277e47bd276838c7306aa7cf67a024feef42f46e f90690fe475fb26ef9d7b2bd96437cf95ec54714f203f8c21fab28cf734ef4ad 3a601546633c927902fb0fc27d6b553a9096eebdea370240a1750d0e2ab00d1f d1dcfecb62864c28e339d6dc4f5512e44e71892e22978ede1126389bdaa16016 a2016a899166a59df4572e88cc36c09ecae437d1b66b0127424dbe2f3b0d857e a2fc69ddf88982773d7fc964630f6659e0b6156ef0a1d1c8144709de6cfe38cc c357ae328d0426aabbc99ed802b0aaa28760d4fdbf51ef0a6a90498c6f472bc6 9cb4597a7d6a7a2d57438b967b03e9b450b8af3b186c7c978fa24b765f157531 2d74509e2779bcc8a5659f12ab6962a0a861b4550b3479d42a74648e50859181 4964e560f2185df5f497d5d4801c97413ed344ebf6c926f92288fc0e5d6214cc 8f4653162ede941bd7991c83198c62307381ee7da0391890f488ca6a84480376 edfd0fafad1b0b7ae8da4a92d7ad2ab16494bd00099372f326221cbba3a39562 df1524e3321fa837671ce5743a9eef0c96d5e8ede4e821ad3f34d3d02ed8c9b4 716ca1340ccf430367129fb48b5ed24ec3fec6ce3d55a47074636f0c3b42daf3 89fe391187d6d71b740e3ffe8e34336cb239c89984036964d02391b2d341626d 591dedd7e8dd4209184f2ec1f7f7afd5001c67a4c117a005a80ce68fae1be1bf 4f11234f2e0ba8c0c121203dc98b73c7ce200b2926dc82511654a98ef2d690b4 2f44f7585f60cc386e332ab2f1f5e1f29d2792e1cf9adbcee4419d0bc7c02f74 de467e954f7cfd1f1423844589ab94855977e067b31eef6fcd6c02ea81ae5090 b9aaddd103eac70b7ebd9d8cb2db921086ecf69759342523af8d7855d3e4d401 2bb77f7ba860c129e66c24e50fc92ebdec37c32563a79dd604299d4ccc7ab1be 4d90314ffbc32453c2d99d19999c3332e705fe794e6318046a77b07fc3a4c590 4ddef396496bfe8a506cefe2511d2269e1ddf3df7125dbdda90d98e2b919f8d8 abfbf892754779e75fd3aa17082e8d9d714dc12fcf1019067937c7f2fc35da52 dead692bed81256262dc69a3cfce87dd0a0351af3bb82df3c8c91f04df47cb83 21919e278184f6c67ffa6c52bd787f68ae86234b0cfeff448df2a1cb76d3b7a7 023b82d8744b9dce246cd13c34922d51cf48dade8316e67ec074884e174f7bea ecb0422c572cb03bb2308be24eeb96a9e52992827e106e8c559f3f35f1d4ff4d d9e0480bd3e04cd437e7a41abd51b6a33e26dbb389d27aef0579c95ce0ca1df1 b1abf944356b4d4f72ff87bd2ef064031cf8f0267a1c12bbebf2689ee0f0dfb9 6bb46a76599aca48cd570f7140dae37966fe6d137a729e3676c7b5ca15beb7be 1ef7ed057d3964ba924c0592eeb2855592151a05382f92a3d5465bae3976ea90 c05dcae387220fa63b23101d303793f706d879ee59e8a8f159507f98d157201b 361c4160173fd5077a2551f41a501bb9ce331cfb4923206f75bf341c579eb3b9 99b7758335e4655767bbe3a4007e197e54e2b753caa6c238ed41772dfd13c7d6
fs/usr/share
S_IFDIR

fs/usr/share/doc
S_IFDIR

fs/usr/share/doc/zstd
S_IFDIR

fs/usr/share/doc/zstd/CHANGELOG
S_IFREG
47290 223ebe3876e6e461b74ac05123897a5c1fbcd016f51e693e9ba5f9e68fd7f7c9 be0034d8c8b35ed7d676fc693e1ebcbf6ee3425ca565ca3c91875d7990c694ed 082450415a95ad0802693f2e4aad56ac0554154352f304129dfb6e4409f02373
fs/usr/share/doc/zstd/README.md
S_IFREG
10934 ead49f64b82039ce16d8a02514b5fee517d275d4f738415e6f0019c727826f8b
fs/usr/share/licenses
S_IFDIR

fs/usr/share/licenses/zstd
S_IFDIR

fs/usr/share/licenses/zstd/COPYING
S_IFREG
18091 68721be0e2e5e985b05b419cb25dd8e9be7139d3cad63f86e4b3334793d37c1b 16bed86d2751edaca2691e685decd7addba5909c3a08a920c6ece35d5eb7f387
fs/usr/share/licenses/zstd/LICENSE
S_IFREG
1549 7055266497633c9025b777c78eb7235af13922117480ed5c674677adc381c9d8"#;

        assert_eq!(b32_hash(content), "g2q62dbhj7g7rn3fggzyihqu2xodq5yf");

    }
}
