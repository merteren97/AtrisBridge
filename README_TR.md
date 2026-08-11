# AtrisBridge

AtrisBridge; yazılım ve mühendislik projelerini farklı bilgisayarlar arasında daha güvenli, takip edilebilir ve taşınabilir hale getirmek için geliştirilen local-first bir masaüstü uygulamasıdır.

> **Durum:** erken alpha (`0.1.0-alpha.7`). Yerel envanter, kalıcı SQLite state, restricted Google Drive transport, guarded backup/restore, conflict-aware two-way sync, işletim sistemi destekli credential persistence, opsiyonel client-side içerik şifreleme ve korumacı continuous watch mode uygulanmıştır.

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
- ilk provider olarak Google Drive kullanan provider-independent mimari.

## Şu anda çalışan özellikler

Phase 0'dan Phase 8'e kadar:

- Tauri 2 + React + TypeScript masaüstü uygulaması,
- native workspace seçimi ve yönetimi,
- Rust scanner + BLAKE3 fingerprint,
- generated output, Git metadata, IDE cache, `.env*`, private key/certificate ve AtrisBridge recovery artifact'ları için built-in exclusions,
- `.atrisbridgeignore`,
- application-data altında SQLite journal,
- kalıcı local/remote evidence,
- pinned rclone `v1.74.4`,
- `drive.file` ile Google Drive OAuth,
- OAuth credential'larını yalnızca işletim sisteminin güvenli credential vault'u üzerinden kalıcı saklama,
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
- başka bilgisayardaki Drive değişiklikleri için bounded provider reconciliation polling,
- yalnızca güvenli transfer planlarında opsiyonel automatic apply,
- conflict, blocked path, scanner belirsizliği veya her türlü deletion olduğunda fail-closed manual review,
- watch loop aktifken manual mutating IPC işlemlerinin backend tarafından engellenmesi,
- plan ve execution öncesi fresh evidence,
- frontend ve Rust doğrulaması için Linux CI.

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

## Phase 8 continuous watch mode

Continuous watch mode tekrar tekrar manuel scan yapma ihtiyacını azaltır fakat planner/executor güvenlik sınırını bypass etmez.

- native filesystem event'leri yalnızca **dirty signal** kabul edilir,
- local event burst'leri 1.8 saniyelik debounce/coalescing penceresinde toparlanır,
- her cycle full scanner + fresh provider observation çalıştırır,
- bounded Drive polling local dosyalar sessizken başka cihazlardan gelen remote değişiklikleri yakalar,
- workspace başına aynı anda yalnızca bir automatic cycle çalışabilir,
- `Auto-apply safe transfers` ayrı bir opt-in ayarıdır ve varsayılan olarak kapalıdır,
- auto-apply kapalıyken güvenli upload/download planları destructive attention yerine **review** olarak bekler,
- conflict, blocked path, eksik scanner evidence, encryption/provider belirsizliği ve her deletion işlemi fail-closed olur,
- Phase 8 **hiçbir deletion action'ını otomatik uygulamaz**,
- watch mode workspace'i sahiplenmişken manual mutating command'lar Rust IPC boundary tarafından reddedilir.

Watch ayarları ve son cycle durumu SQLite'ta kalıcıdır. Uygulama yeniden açıldığında configured watcher'lar ancak interrupted-transfer recovery tamamlandıktan sonra resume edilir.

Scheduler/state/retry ayrıntıları için [docs/continuous-watch.md](docs/continuous-watch.md) belgesine bakabilirsiniz.

## Secure credential storage

Google Drive OAuth credential'ları işletim sisteminin secure credential facility'sinde tutulur ve gerektiğinde Rust backend tarafından lazy-load edilir.

Credential şu yerlere yazılmaz:

- SQLite,
- `rclone.conf`,
- `.env`,
- synchronized workspace,
- repository dosyaları.

Saved credential kaldırılırsa cloud işlemlerine devam etmek için Google OAuth bağlantısını yeniden kurmak gerekir. Provider'ı **Forget** etmek AtrisBridge provider metadata'sını da kaldırır; Drive verisini silmez.

## Opsiyonel client-side encryption

Client-side encryption workspace bazında opt-in'dir. Yalnızca henüz accepted synchronized baseline oluşmamışken etkinleştirilebilir ve managed remote root boş olmalıdır. AtrisBridge plaintext veriyi yerinde otomatik olarak ciphertext'e migrate etmez.

Encryption açıkken:

- regular file içeriği Drive'a gitmeden önce local'de şifrelenir,
- restore/sync sırasında plaintext local tarafta üretilir,
- encryption master key `AB1-...` recovery key ile temsil edilir,
- recovery key OS credential vault'ta saklanır,
- recovery key yalnızca explicit export/import aksiyonlarıyla kullanıcıya açılır,
- local BLAKE3 ile remote ciphertext provider ID/MD5 evidence ayrı tutulur,
- encrypted Drive object reviewed Trash sırasında yine exact provider ID ile hedeflenir.

### Metadata sınırı

İlk encrypted transport sürümünde filename encryption bilinçli olarak kapalıdır. **Dosya içeriği şifrelidir; ancak dosya adları ve klasör yapısı storage provider tarafından görülebilir.** Bu tercih mevcut exact path/provider-ID/collision/conflict/delete evidence modelini korur.

Encrypted namespace veya key-verification sentinel kaybolur/bozulursa AtrisBridge bunu "remote boş" veya "remote silindi" olarak yorumlamaz; fail-closed davranıp yeni delete intent üretmez.

## Yol haritası

1. **Phase 0/1 — temel mimari ve local inventory** ✅
2. **Phase 2 — SQLite sync journal ve kalıcı file state** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — güvenli incremental backup** ✅
5. **Phase 5 — güvenli pull ve restore** ✅
6. **Phase 6 — conflict-aware two-way sync** ✅
7. **Phase 7 — persistent secure credential storage + opsiyonel client-side content encryption** ✅
8. **Phase 8 — continuous watch mode + korumacı scheduler** ✅
9. **Phase 9+ — tray/progress/notifications, ek provider'lar ve cross-platform release pipeline**

Detaylar için [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md), [docs/security.md](docs/security.md), [docs/continuous-watch.md](docs/continuous-watch.md), [docs/rclone-transport.md](docs/rclone-transport.md), [docs/backup-engine.md](docs/backup-engine.md) ve [docs/restore-engine.md](docs/restore-engine.md) belgelerine bakabilirsiniz.

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
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo check --manifest-path src-tauri/Cargo.toml
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
