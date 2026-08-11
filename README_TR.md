# AtrisBridge

AtrisBridge; yazılım ve mühendislik projelerini farklı bilgisayarlar arasında daha güvenli, takip edilebilir ve taşınabilir hale getirmek için geliştirilen local-first bir masaüstü uygulamasıdır.

> **Durum:** erken alpha (`0.1.0-alpha.5`). Yerel envanter, kalıcı SQLite state, restricted Google Drive transport, guarded backup, verified restore ve explicit conflict-aware two-way synchronization uygulanmıştır. Sürekli arka plan sync'i ve otomatik conflict resolution bilinçli olarak kapalıdır.

[English README](README.md)

## Neden AtrisBridge?

Aktif projeleri bilgisayarlar arasında taşımak çoğu zaman ZIP dosyaları, manuel cloud klasörleri, eski kopyalar ve hangi sürümün güncel olduğuna dair belirsizlik oluşturur. AtrisBridge local proje ile storage provider arasına korumacı bir koordinasyon katmanı koyar:

- local-first workspace metadata ve scanning,
- restart sonrasında kaybolmayan SQLite file state,
- local içerik için BLAKE3 fingerprint,
- remote provider ID/size/checksum evidence'ını ayrı tutma,
- explicit last-synchronized baseline,
- secret/generated dosyalar için built-in ignore + `.atrisbridgeignore`,
- synchronized path'lerde symlink traversal yapmama,
- review-first backup, restore ve two-way planları,
- kör kalıcı silme yerine recoverable deletion semantics.

## Şu anda çalışan özellikler

Phase 0'dan Phase 6'ya kadar ilk tam reviewed synchronization loop tamamlandı:

- Tauri 2 + React + TypeScript masaüstü shell,
- workspace yönetimi ve native directory picker,
- Rust scanner + BLAKE3 fingerprint,
- OS application-data altında SQLite state,
- kalıcı local / remote / last-synchronized evidence,
- tam olarak `v1.74.4` isteyen pinned rclone runtime,
- `drive.file` scope ile Google Drive OAuth; OAuth session verisi yalnızca process memory'de,
- workspace → managed Drive folder binding,
- guarded local → Drive backup,
- verified Drive → local restore,
- explicit **Two-Way** workspace modu,
- planlama ve execution öncesinde fresh local + remote observation,
- baseline tabanlı upload/download kararları,
- modify/modify ve delete/modify conflict'lerini last-write-wins kullanmadan gösterme,
- reviewed local deletion → exact reviewed Google Drive file ID'yi Trash'e taşıma,
- reviewed remote deletion → local silmeden önce verified recovery copy oluşturma,
- kullanıcı tarafından local'e geri yüklenebilen recovery copy'ler,
- deletion propagation çevresinde live local/remote absence preflight,
- interrupted transfer/apply state'leri için startup recovery,
- iki aşamalı desktop akış: **Prepare → review → Run**,
- frontend ve Rust için Linux CI.

Workspace kaldırmak yalnızca AtrisBridge metadata'sını siler; proje dizinine dokunmaz. Provider'ı unutmak da local provider metadata'sını ve memory'deki session'ı kaldırır; Drive verisini silmez.

## Phase 6 — conflict-aware two-way synchronization

Two-Way davranışı workspace bazında explicit olarak açılır. Modu açmak tek başına hiçbir transfer başlatmaz. **Prepare sync** iki inventory'yi yenileyip review edilebilir bir plan oluşturur; yalnızca **Run sync** hâlâ güvenli olduğu doğrulanan item'ları uygular.

AtrisBridge current local/remote evidence'ı son başarılı synchronized baseline ile karşılaştırır:

| Local | Remote | Baseline yorumu | Karar |
| --- | --- | --- | --- |
| yeni | yok | baseline yok | upload create |
| yok | yeni | baseline yok | download create |
| var | var | baseline yok | unverified overlap, block |
| değişmiş | aynı | verified | upload update |
| aynı | değişmiş | verified | download update |
| değişmiş | değişmiş | verified | conflict; iki tarafa da dokunma |
| silinmiş | aynı | verified | reviewed Drive file ID → Trash |
| silinmiş | değişmiş | verified | delete/modify conflict |
| aynı | silinmiş | verified | recoverable local delete |
| değişmiş | silinmiş | verified | delete/modify conflict |
| silinmiş | silinmiş | verified | converged deletion acknowledgement |

Modification time conflict authority olarak kullanılmaz. Evidence eksik, belirsiz, ignored, unsafe veya case-insensitive filesystem'de çakışıyorsa AtrisBridge tahmin yapmak yerine item'ı bloke eder.

### Deletion safety

**Local deletion → Drive:** current remote ID, size ve MD5 synchronized evidence ile hâlâ eşleşmelidir. Provider mutation'dan hemen önce local path'in hâlâ gerçekten absent olduğu tekrar kontrol edilir. Reviewed Google Drive object **exact file ID** üzerinden Trash'e taşınır; permanent delete surface yoktur. Postflight path kontrolü, aynı path'e sonradan gelen başka bir Drive object'in silinen object sanılmasını engeller.

**Remote deletion → local:** local BLAKE3 + size baseline ile hâlâ eşleşmelidir ve targeted Drive kontrolleri remote path'in absent kalmaya devam ettiğini doğrulamalıdır. Local dosya kaldırılmadan önce AtrisBridge application-data recovery alanında bir copy oluşturur, BLAKE3 + size ile doğrular, diske flush eder, `applying` state'ini yazar ve ancak bundan sonra workspace dosyasını kaldırır. Recovery metadata, deletion convergence ve operation completion aynı SQLite transaction'ında commit edilir.

Available recovery copy'ler Two-Way panelinde görünür. **Restore locally** app-data recovery dosyasını yeniden doğrular, mevcut bir local path'in üzerine yazmayı reddeder, dosyayı local-only change olarak geri oluşturur ve Google Drive'ı değiştirmez. Sonraki reviewed sync plan bu yeniden oluşturulan dosya için kararı verir.

### Provider race boundary

AtrisBridge content write veya Trash için provider-native atomic compare-and-swap garantisi verdiğini iddia etmez. Fresh inventory, targeted ID/checksum preflight, live absence check, exact-ID Trash, postflight verification ve recoverable local mutation race window'larını daraltır; ancak başka bir Drive client ayrı provider request'leri arasında object'i değiştirebilir. Trash recoverable olduğu ve conflict'ler otomatik çözülmediği için belirsiz state fail-closed olur ve fresh reviewed plan gerektirir.

Direct Drive Trash control-plane request current in-memory OAuth access token'ını kullanır. Access token artık geçerli değilse operation güvenli şekilde fail olur ve provider yeniden bağlanabilir; AtrisBridge plaintext OAuth credential persist etmez.

Detay için [docs/sync-engine.md](docs/sync-engine.md) ve [docs/rclone-transport.md](docs/rclone-transport.md).

## Phase 4/5 one-way safety

Workspace Two-Way modunda değilken backup ve restore explicit one-way workflow olarak kullanılmaya devam eder:

- **Backup:** yalnızca local → Drive; local deletion remote deletion anlamına gelmez.
- **Restore:** yalnızca Drive → local; remote absence local deletion anlamına gelmez.
- Restore download önce hidden staging path'e alınır ve local apply öncesi doğrulanır.
- Existing local restore target journal completion'a kadar geçici `.bak` recovery copy kullanır.

Detay için [docs/backup-engine.md](docs/backup-engine.md) ve [docs/restore-engine.md](docs/restore-engine.md).

## Yol haritası

1. **Phase 0/1 — temel mimari ve local inventory** ✅
2. **Phase 2 — SQLite sync journal ve kalıcı file state** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — güvenli incremental backup** ✅
5. **Phase 5 — güvenli pull ve restore** ✅
6. **Phase 6 — conflict-aware two-way synchronization** ✅
7. **Phase 7 — kalıcı secure credential storage + opsiyonel client-side encryption**
8. **Phase 8+ — continuous watch mode, tray, ek provider'lar ve release pipeline**

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

`sidecar:prepare`, resmi rclone release host'undan `v1.74.4` arşivini indirir, platform SHA-256 değerini doğrular ve executable'ı `src-tauri/binaries/` altına yerleştirir. Binary Git tarafından ignore edilir.

Validation:

```bash
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo check --manifest-path src-tauri/Cargo.toml
```

## Google Drive davranışı

Google Drive browser-based OAuth ve `drive.file` scope kullanır. Provider operations `AtrisBridge/...` managed workspace path'i ile sınırlandırılır. Regular transfer bytes narrow rclone operations arkasında kalır; Phase 6 yalnızca exact reviewed file ID'yi Trash'e taşımak için dar kapsamlı direct Drive API control-plane request kullanır.

Remote MD5 provider evidence olarak tutulur ve BLAKE3 ile aynı algoritmaymış gibi karşılaştırılmaz. Current regular-file adapter native Google Docs objelerini atlar.

OAuth token yalnızca process memory'dedir. Uygulama restart sonrasında Phase 7 secure credential layer gelene kadar provider'ın yeniden bağlanması gerekir.

## `.atrisbridgeignore`

Workspace root'ta gitignore uyumlu `.atrisbridgeignore` kullanılabilir. Custom dosya olmasa bile built-in safety rules aktiftir.

```gitignore
artifacts/
cache/
customer-dumps/
*.bak
```

Built-in kurallar Git metadata, yaygın generated/IDE dizinleri, `.env*`, common private-key/certificate formatları ve AtrisBridge `.part`/`.bak` transfer-recovery artifact'larını dışlar.

## Kalıcı sync journal

AtrisBridge coordination state'ini OS application-data dizinindeki `atrisbridge.db` içinde tutar. SQLite foreign key, WAL journaling ve bounded busy timeout kullanır. Local observation, remote observation, synchronized baseline, plan, conflict ve recovery metadata ayrı evidence sınıfları olarak tutulur.

Phase 5 ve Phase 6 feature-owned tabloları mevcut Phase 4 core tablolarını destructive rewrite etmeden idempotent ekler. Interrupted operation sessizce retry edilmez veya synchronized kabul edilmez; startup recovery yalnızca stored fingerprint'in güvenli olduğunu kanıtladığı mutation'ları rollback eder.

## Güvenlik ve şirket politikası

AtrisBridge yanlışlıkla veri sızdırma riskini azaltacak şekilde tasarlanır; ancak şirket veya müşteri kaynak kodunu üçüncü taraf bir servise yüklemek için size yetki vermez. Senkronize ettiğiniz projenin şirket politikalarına, DLP kurallarına, sözleşmelere, data-residency/export-control gereksinimlerine ve yetkilendirme kurallarına her zaman uymalısınız.

Güvenlik açıklarını public issue üzerinden paylaşmayın. [SECURITY.md](SECURITY.md) belgesini kullanın.

## Lisans

[Apache License 2.0](LICENSE) ile lisanslanmıştır.
