# AtrisBridge

AtrisBridge; yazılım ve mühendislik projelerini farklı bilgisayarlar arasında daha güvenli, takip edilebilir ve taşınabilir hale getirmek için geliştirilen local-first bir masaüstü uygulamasıdır.

> **Durum:** erken alpha (`0.1.0-alpha.4`). Yerel envanter, kalıcı SQLite state, restricted Google Drive observation, guarded backup ve explicit verified restore uygulanmıştır. Remote deletion ve otomatik two-way sync bilinçli olarak kapalıdır.

[English README](README.md)

## Neden AtrisBridge?

Aktif projeleri bilgisayarlar arasında taşımak çoğu zaman ZIP dosyaları, manuel cloud klasörleri, eski kopyalar ve hangi sürümün güncel olduğuna dair belirsizlik oluşturur. AtrisBridge yerel proje ile depolama sağlayıcısı arasına güvenli bir katman koymak için tasarlanır:

- local-first workspace yönetimi,
- uygulama yeniden başlatıldığında kaybolmayan SQLite dosya state'i,
- kaynak kod ve secret dosyalar için korumacı ignore kuralları,
- `.atrisbridgeignore` desteği,
- BLAKE3 içerik fingerprint'leri,
- inventory ve restore path çözümünde symlink takip etmeme,
- evidence-first conflict ve tombstone yönetimi,
- ilk provider olarak Google Drive kullanan provider-independent transport mimarisi.

## Şu anda çalışan özellikler

Phase 0'dan Phase 5'e kadar local state, cloud observation, guarded backup ve explicit restore tamamlandı:

- Tauri 2 + React + TypeScript masaüstü uygulaması,
- workspace ekleme/kaldırma ve native klasör seçici,
- Rust scanner ve BLAKE3 fingerprint'leri,
- generated output, Git metadata, IDE cache, `.env*` ve yaygın key/certificate formatları için built-in exclude kuralları,
- `.atrisbridgeignore`,
- application-data altında SQLite veritabanı,
- kalıcı local scan geçmişi ve tam dosya envanteri,
- local / remote / last-synchronized evidence modeli,
- restart-safe state ve tombstone altyapısı,
- tam olarak `v1.74.4` isteyen pinned rclone runtime,
- `drive.file` ile Google Drive OAuth,
- OAuth token'ını yalnızca process memory'de tutma,
- workspace → Drive folder binding,
- remote ID/checksum observation'larını BLAKE3'ten ayrı saklama,
- guarded local → Drive backup planlama ve execution,
- explicit Drive → local restore planlama ve execution,
- plan öncesi ve execution öncesi fresh local + remote observation,
- dosya bazlı safe action / blocked kararı,
- staged restore download + remote MD5/size doğrulaması,
- mevcut local dosyalar için recoverable `.bak` update modeli,
- uygulama kapanmasıyla yarım kalan transferler için startup recovery,
- iki aşamalı UI: **Prepare → review → Run**,
- frontend ve Rust doğrulaması için Linux CI.

Workspace kaldırmak yalnızca AtrisBridge metadata'sını siler; proje dizinine dokunmaz. Provider'ı unutmak da Google Drive'daki hiçbir veriyi silmez.

## Phase 4 güvenli backup modeli

Phase 4 bilinçli olarak **local → Google Drive** yönünde çalışır. **Prepare plan** fresh local ve remote observation oluşturur fakat hiçbir dosya upload etmez. Gerçek write işlemleri yalnızca `AtrisBridge/...` altında yönetilen remote path'lerde çalışır. **Run backup** başlamadan önce iki inventory yeniden güncellenir ve her item tekrar doğrulanır.

Yeni objelerde upload'dan hemen önce targeted existence check yapılır ve single-file `copyto --immutable` kullanılır. Mevcut remote obje yalnızca remote ID/checksum bilinen AtrisBridge baseline'ı ve plan evidence'ıyla eşleşmeye devam ediyorsa update edilebilir.

Transfer sırasında local dosyanın değişmediği BLAKE3 ile tekrar doğrulanır. Transfer sonrası Google Drive size ve MD5 değeri local evidence ile birebir eşleşmeden SQLite'a yeni synchronized baseline yazılmaz.

Local deletion hiçbir zaman remote delete'e çevrilmez. Remote-only dosyalar, duplicate remote path'ler, conflict'ler ve AtrisBridge baseline'ı olmayan overlap'lar değiştirilmek yerine bloke edilir.

Ayrıntılı model için [docs/backup-engine.md](docs/backup-engine.md) belgesine bakın.

## Phase 5 güvenli restore modeli

Phase 5 explicit **Google Drive → local** restore yolu ekler. Bu otomatik çalışan bir pull loop değildir ve remote'daki bir dosyanın kaybolmasını hiçbir zaman local dosyayı silme izni olarak yorumlamaz.

Restore planner; remote-only dosyayı güvenli local `create`, remote tarafta değişmiş bir dosyayı ise yalnızca mevcut local dosya son synchronized BLAKE3 baseline'ıyla hâlâ eşleşiyorsa güvenli local `update` olarak sınıflandırır. Local değişiklik, iki tarafın birden değişmesi, baseline olmayan overlap, unsafe path, eksik provider evidence ve case-insensitive dosya adı collision durumları manuel inceleme için bloke edilir.

rclone final proje dosyasının üzerine doğrudan download yapmaz. AtrisBridge içeriği önce benzersiz hidden staging dosyasına indirir, Google Drive size + MD5 değerleriyle doğrular, final remote stat yapar, local target'ı yeniden kontrol eder ve ancak bundan sonra içeriği uygular. Mevcut local dosya değiştirilmeden önce recoverable `.bak` dosyasına taşınır; bu recovery copy ancak SQLite journal commit'i başarılı olduktan sonra kaldırılır.

AtrisBridge local apply sırasında kapanırsa startup recovery yalnızca downloaded BLAKE3 + size rollback'in güvenli olduğunu kanıtlıyorsa geri alır. Belirsiz dosyalar otomatik overwrite edilmek yerine korunur.

Phase 5 regular file içeriğini restore eder; Unix executable bit, ACL, ownership, alternate data stream veya provider-specific filesystem metadata'nın platformlar arasında birebir restore edileceğini garanti etmez.

Ayrıntılı model için [docs/restore-engine.md](docs/restore-engine.md) ve [docs/rclone-transport.md](docs/rclone-transport.md) belgelerine bakın.

## Yol haritası

1. **Phase 0/1 — temel mimari ve local inventory** ✅
2. **Phase 2 — SQLite sync journal ve kalıcı dosya state'i** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — güvenli incremental backup** ✅
5. **Phase 5 — güvenli pull ve restore** ✅
6. **Phase 6 — conflict-aware two-way sync**
7. **Phase 7 — kalıcı secure credential storage + opsiyonel client-side encryption**
8. **Phase 8+ — sürekli izleme, tray, ek provider'lar ve release pipeline**

Detaylar için [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md), [docs/security.md](docs/security.md), [docs/rclone-transport.md](docs/rclone-transport.md), [docs/backup-engine.md](docs/backup-engine.md) ve [docs/restore-engine.md](docs/restore-engine.md) belgelerine bakabilirsiniz.

## Geliştirme

### Gereksinimler

- Node.js LTS
- npm
- Rust stable
- İşletim sisteminiz için Tauri 2 gereksinimleri

AtrisBridge sistem `PATH`'inden rastgele bir `rclone` executable çalıştırmaz:

```bash
npm install
npm run sidecar:prepare
npm run tauri:dev
```

`sidecar:prepare`, resmi rclone release host'undan `v1.74.4` arşivini indirir, platforma ait SHA-256 değerini doğrular ve executable'ı `src-tauri/binaries/` altına yerleştirir. Binary Git tarafından ignore edilir.

Frontend validation:

```bash
npm run build
```

Rust validation:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo check --manifest-path src-tauri/Cargo.toml
```

## Google Drive davranışı

Google Drive bağlantısı browser tabanlı OAuth ve `drive.file` scope kullanır. Backup ve restore işlemleri ayrıca `AtrisBridge/...` yönetilen workspace path'i ile sınırlandırılmıştır.

OAuth token yalnızca process memory'de tutulur. Uygulama yeniden açıldığında provider'a yeniden bağlanmak gerekir. Secure credential layer gelmeden plaintext token saklamak bilinçli olarak yapılmaz.

Remote provider checksum'ları (örneğin MD5) BLAKE3 ile aynı şeymiş gibi karşılaştırılmaz. Doğrulanmış transfer sonrasında synchronized baseline local BLAKE3 ve provider evidence'ını ayrı alanlarda tutar.

Current Drive adapter native Google Docs objelerini atlar; Phase 4/5 regular file content ve provider checksum evidence ile çalışır.

## `.atrisbridgeignore`

Workspace kökünde `.atrisbridgeignore` kullanarak gitignore uyumlu proje kuralları ekleyebilirsiniz. Custom dosya bulunmasa bile built-in güvenlik kuralları aktif kalır.

```gitignore
artifacts/
cache/
customer-dumps/
*.bak
```

## Kalıcı sync journal

AtrisBridge state'ini işletim sisteminin application-data dizinindeki `atrisbridge.db` dosyasında saklar. Local ve remote observation'lar ayrı tutulur; transport katmanı yalnızca modification time'a bakarak karar vermez.

Core database Phase 4'teki schema v3 olarak kalır. Phase 5 restore-plan tablolarını mevcut Phase 4 tablolarını rewrite etmeden idempotent olarak ekler. Yarım kalan backup/restore execution sessizce retry edilmez veya synchronized kabul edilmez; sonraki deneme fresh evidence ve yeni plan gerektirir.

## Güvenlik ve şirket politikası

AtrisBridge yanlışlıkla veri sızdırma riskini azaltacak şekilde tasarlanır; ancak şirket veya müşteri kaynak kodunu üçüncü taraf bir servise yüklemek için size yetki vermez. Senkronize ettiğiniz projenin şirket politikalarına, DLP kurallarına, sözleşmelere, data-residency/export-control gereksinimlerine ve yetkilendirme kurallarına her zaman uymalısınız.

Güvenlik açıklarını public issue üzerinden paylaşmayın. [SECURITY.md](SECURITY.md) belgesini kullanın.

## Lisans

[Apache License 2.0](LICENSE) ile lisanslanmıştır.
