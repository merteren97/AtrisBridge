# AtrisBridge

AtrisBridge; yazılım ve mühendislik projelerini farklı bilgisayarlar arasında daha güvenli, takip edilebilir ve taşınabilir hale getirmek için geliştirilen local-first bir masaüstü uygulamasıdır.

> **Durum:** erken alpha (`0.1.0-alpha.9`). Yerel envanter, kalıcı SQLite state, restricted Google Drive transport, guarded backup/restore, conflict-aware two-way sync, OS-backed credential persistence, opsiyonel client-side içerik şifreleme, continuous watch, remembered AtrisHub desktop session, system tray runtime, global sync activity, signed updater ve Windows/Linux release packaging uygulanmıştır.

[English README](README.md)

## Neden AtrisBridge?

Aktif projeleri bilgisayarlar arasında taşımak çoğu zaman ZIP dosyaları, manuel cloud klasörleri, eski kopyalar ve hangi sürümün güncel olduğuna dair belirsizlik oluşturur. AtrisBridge local proje ile storage provider arasına korumacı bir koordinasyon katmanı koyar:

- local-first workspace metadata ve scanning,
- restart sonrasında kaybolmayan SQLite file state,
- local içerik için BLAKE3 fingerprint,
- local / remote / synchronized-baseline evidence ayrımı,
- last-write-wins yerine conflict-aware two-way planlama,
- recoverable deletion propagation,
- OS-native secure credential storage,
- opsiyonel client-side içerik şifreleme,
- filesystem event'lerini sync gerçeği değil yalnızca dirty signal olarak kullanan continuous reconciliation,
- unattended watch için tray'de yaşamaya devam eden desktop runtime,
- ilk provider olarak Google Drive kullanan provider-independent mimari.

## Şu anda çalışan özellikler

Phase 0'dan Phase 9'a kadar:

- Tauri 2 + React + TypeScript masaüstü uygulaması,
- native workspace seçimi ve yönetimi,
- Rust scanner + BLAKE3 fingerprint,
- generated output, Git metadata, IDE cache, `.env*`, private key/certificate ve recovery artifact'ları için built-in exclusions,
- `.atrisbridgeignore`,
- application-data altında SQLite journal,
- kalıcı local/remote evidence,
- pinned rclone `v1.74.4`,
- `drive.file` ile Google Drive OAuth,
- OAuth credential'larını yalnızca OS credential vault üzerinden kalıcı saklama,
- workspace → Drive folder binding,
- guarded local → Drive backup,
- staged ve recoverable Drive → local restore,
- review-first conflict-aware two-way synchronization,
- reviewed deletion için exact-ID Google Drive Trash,
- remote deletion local'e uygulanmadan önce app-data recovery copy,
- recovery copy'yi açık kullanıcı aksiyonuyla geri alma,
- workspace bazında opsiyonel **client-side içerik şifreleme**,
- encrypted workspace için `AB1-...` recovery key export/import,
- workspace bazında native filesystem watcher + debounce/coalescing,
- başka bilgisayardaki Drive değişiklikleri için bounded provider reconciliation,
- yalnızca güvenli transfer planlarında opsiyonel automatic apply,
- conflict, blocked path, scanner belirsizliği veya her deletion durumunda fail-closed manual review,
- watch loop aktifken manual mutating IPC işlemlerinin backend tarafından engellenmesi,
- remembered AtrisHub desktop account ve OS vault içinde rotating refresh credential,
- Open / Hide / Quit aksiyonlarına sahip system tray,
- pencere kapatıldığında configured watcher'ların çalışmaya devam edebilmesi için close-to-tray davranışı,
- active cycle, queued operation, conflict ve workspace watcher durumunu gösteren global Activity Center,
- opt-in desktop alert + in-app fallback,
- preview/stable kanal desteğine sahip signed Tauri updater,
- owner-controlled Windows x64 ve Linux x64 package/release workflow'ları,
- reproducible npm/Cargo lockfile ve CI doğrulaması.

Workspace'i AtrisBridge'den kaldırmak proje dosyalarını silmez. Encryption recovery key'i de workspace metadata ile birlikte OS credential vault'tan otomatik silinmez; sessiz key silme encrypted remote veriyi geri döndürülemez hale getirebileceği için fail-safe şekilde korunur.

## Sync güvenlik modeli

AtrisBridge modification time'ı conflict otoritesi olarak kullanmaz ve arka planda last-write-wins çalıştırmaz.

Two-Way modunda:

- local değişti / remote aynı → upload,
- local aynı / remote değişti → download,
- iki taraf da değişti → conflict,
- local silindi / remote aynı → exact reviewed Drive object Trash'e taşınır,
- remote silindi / local aynı → verified recovery copy sonrası reviewed local deletion,
- delete/modify overlap → conflict,
- shared baseline sonrasında iki taraf da yok → converged deletion acknowledgement.

Execution öncesinde provider ve filesystem evidence yeniden okunur; SQLite completion exact plan evidence üzerinden conditional yapılır.

## Continuous watch ve desktop runtime

Continuous watch mode tekrar tekrar manuel scan yapma ihtiyacını azaltır fakat planner/executor güvenlik sınırını bypass etmez.

- native filesystem event'leri yalnızca **dirty signal** kabul edilir,
- local event burst'leri debounce/coalescing penceresinde toparlanır,
- her cycle full scanner + fresh provider observation çalıştırır,
- bounded Drive polling local dosyalar sessizken başka cihazlardan gelen remote değişiklikleri yakalar,
- workspace başına aynı anda yalnızca bir automatic cycle çalışabilir,
- `Auto-apply safe transfers` ayrı bir opt-in ayarıdır ve varsayılan olarak kapalıdır,
- conflict, blocked path, eksik scanner evidence, encryption/provider belirsizliği ve her deletion işlemi fail-closed olur,
- watch mode **hiçbir deletion action'ını otomatik uygulamaz**,
- watch mode workspace'i sahiplenmişken manual mutating command'lar Rust IPC boundary tarafından reddedilir.

Desktop'ta ana pencereyi kapatmak AtrisBridge sürecini sonlandırmak yerine uygulamayı system tray'e gizler. Tray'deki explicit **Quit AtrisBridge** aksiyonu gerçek çıkış noktasıdır. Activity Center aynı durable journal/runtime state'i gözlemler; ikinci bir sync authority oluşturmaz ve review kapılarını bypass etmez.

Ayrıntılar: [docs/continuous-watch.md](docs/continuous-watch.md) ve [docs/desktop-runtime.md](docs/desktop-runtime.md).

## Secure credential ve AtrisHub account

Provider credential'ları, encryption secret'ları ve remembered AtrisHub refresh credential'ları secret oldukları sürece React, SQLite, `.env`, synchronized workspace veya repository dosyalarına yazılmaz. Kalıcı secret'lar için OS-backed secure storage kullanılır.

AtrisHub login opsiyoneldir; account olmadan local AtrisBridge akışları çalışmaya devam eder. Remembered session rotating refresh credential kullanırken kısa ömürlü access credential process memory içinde tutulur.

Ayrıntılar: [docs/security.md](docs/security.md) ve [docs/atrishub-account.md](docs/atrishub-account.md).

## Opsiyonel client-side encryption

Client-side encryption workspace bazında opt-in'dir ve yalnızca accepted synchronized baseline oluşmadan, managed remote root boşken etkinleştirilebilir. AtrisBridge plaintext veriyi yerinde otomatik olarak ciphertext'e migrate etmez.

Regular file içeriği Drive'a gitmeden önce local'de şifrelenir. İlk encrypted transport sürümünde filename encryption bilinçli olarak kapalıdır; bu nedenle **dosya içeriği şifrelidir fakat dosya adları ve klasör yapısı storage provider tarafından görülebilir**. Missing/corrupt encrypted namespace veya key-verification evidence güvenli olmayan provider state olarak değerlendirilir ve fail-closed olur.

## Release ve updater

Release foundation Windows x64 NSIS/MSI ve Linux x64 AppImage/DEB paketleri üretir. rclone binary olarak repoya commit edilmez; packaging öncesinde pinned sürüm indirilip SHA-256 ile doğrulanır. Tauri updater signed updater artifact'larını ve AtrisHub channel policy'yi kullanırken package byte'ları GitHub Releases üzerinde kalır.

Ayrıntılar: [docs/release-updater.md](docs/release-updater.md).

## Yol haritası

1. **Phase 0/1 — temel mimari ve local inventory** ✅
2. **Phase 2 — SQLite sync journal ve kalıcı file state** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — güvenli incremental backup** ✅
5. **Phase 5 — güvenli pull ve restore** ✅
6. **Phase 6 — conflict-aware two-way sync** ✅
7. **Phase 7 — persistent secure credential storage + opsiyonel client-side content encryption** ✅
8. **Phase 8 — continuous watch mode + korumacı scheduler** ✅
9. **Phase 9 — tray lifecycle, activity/progress UX, alert'ler, AtrisHub desktop session ve signed Windows/Linux release foundation** ✅
10. **Phase 10+ — ek storage provider'lar, daha geniş platform packaging ve sonraki ürün entegrasyonları**

Mimari ve subsystem ayrıntıları [`docs/`](docs/architecture.md) altında bulunur.

## Geliştirme

Gereksinimler:

- Node.js LTS
- npm
- Rust stable
- işletim sisteminiz için Tauri 2 gereksinimleri

Pinned rclone sidecar'ı hazırlayın:

```bash
npm install
npm run sidecar:prepare
npm run tauri:dev
```

Validation:

```bash
npm run build
npm run test:release-contract
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib
cargo check --locked --manifest-path src-tauri/Cargo.toml
```

## `.atrisbridgeignore`

Workspace kökünde gitignore-compatible `.atrisbridgeignore` kuralları eklenebilir. Built-in safety exclusions custom dosya bulunmasa da aktiftir.

```gitignore
artifacts/
cache/
customer-dumps/
*.bak
```

## Güvenlik ve şirket politikası

AtrisBridge yanlışlıkla veri sızdırma ve destructive synchronization riskini azaltacak şekilde tasarlanır; ancak şirket veya müşteri kaynak kodunu üçüncü taraf bir servise yüklemek için size yetki vermez. Senkronize edilen projenin şirket politikasına, DLP kurallarına, sözleşmelere, data-residency/export-control ve yetkilendirme gereksinimlerine her zaman uyulmalıdır.

Güvenlik açıklarını public issue üzerinden paylaşmayın. [SECURITY.md](SECURITY.md) belgesini kullanın.

## Lisans

[Apache License 2.0](LICENSE) ile lisanslanmıştır.
