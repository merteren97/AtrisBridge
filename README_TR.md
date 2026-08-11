# AtrisBridge

AtrisBridge; yazılım ve mühendislik projelerini farklı bilgisayarlar arasında daha güvenli, takip edilebilir ve taşınabilir hale getirmek için geliştirilen local-first bir masaüstü uygulamasıdır.

> **Durum:** erken alpha (`0.1.0-alpha.3`). Yerel envanter, kalıcı SQLite state, restricted Google Drive observation ve ilk güvenli local → cloud backup write path uygulanmıştır. Pull, remote delete ve two-way sync bilinçli olarak kapalıdır.

[English README](README.md)

## Neden AtrisBridge?

Aktif projeleri bilgisayarlar arasında taşımak çoğu zaman ZIP dosyaları, manuel cloud klasörleri, eski kopyalar ve hangi sürümün güncel olduğuna dair belirsizlik oluşturur. AtrisBridge yerel proje ile depolama sağlayıcısı arasına güvenli bir katman koymak için tasarlanır:

- local-first workspace yönetimi,
- uygulama yeniden başlatıldığında kaybolmayan SQLite dosya state'i,
- kaynak kod ve secret dosyalar için korumacı ignore kuralları,
- `.atrisbridgeignore` desteği,
- BLAKE3 içerik fingerprint'leri,
- tarama sırasında symlink takip etmeme,
- evidence-first conflict ve tombstone yönetimi,
- ilk provider olarak Google Drive kullanan provider-independent transport mimarisi.

## Şu anda çalışan özellikler

Phase 0'dan Phase 4'e kadar local state, cloud observation ve ilk guarded write path tamamlandı:

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
- SQLite schema v3 backup planları,
- plan öncesi ve execution öncesi fresh local + remote observation,
- dosya bazlı `create`, `update` veya `blocked` kararı,
- update öncesi targeted remote ID/checksum preflight,
- transfer boyunca local BLAKE3 + MD5 evidence,
- baseline kabul edilmeden önce remote size + MD5 doğrulaması,
- uygulama kapanmasıyla yarım kalan backup için startup recovery,
- iki aşamalı UI: **Prepare plan → review → Run backup**,
- frontend ve Rust doğrulaması için Linux CI.

Workspace kaldırmak yalnızca AtrisBridge metadata'sını siler; proje dizinine dokunmaz. Provider'ı unutmak da Google Drive'daki hiçbir veriyi silmez.

## Phase 4 güvenli backup modeli

Phase 4 bilinçli olarak yalnızca **local → Google Drive** yönünde çalışır. Download, remote delete, move, purge, bisync, mount, serve, rclone RC veya generic rclone command surface yoktur.

**Prepare plan** fresh local ve remote observation oluşturur fakat hiçbir dosya upload etmez. Gerçek write işlemleri yalnızca `AtrisBridge/...` altında yönetilen remote path'lerde çalışır. **Run backup** başlamadan önce iki inventory yeniden güncellenir ve her item tekrar doğrulanır.

Yeni objelerde upload'dan hemen önce targeted existence check yapılır ve single-file `copyto --immutable` kullanılır. Mevcut remote obje yalnızca remote ID/checksum bilinen AtrisBridge baseline'ı ve plan evidence'ıyla eşleşmeye devam ediyorsa update edilebilir.

Transfer sırasında local dosyanın değişmediği BLAKE3 ile tekrar doğrulanır. Transfer sonrası Google Drive size ve MD5 değeri local evidence ile birebir eşleşmeden SQLite'a yeni synchronized baseline yazılmaz.

Mevcut Drive objesi update edilirken rclone adapter atomik compare-and-swap garantisi verdiğini iddia etmez. Final targeted preflight ile write arasında küçük bir provider-side race window kalabilir. Bu sınır açıkça belgelenmiştir; uygun conditional-write mekanizması bulunan direct provider adapter ileride bu aralığı kapatabilir.

Local deletion Phase 4'te hiçbir zaman remote delete'e çevrilmez. Remote-only dosyalar, duplicate remote path'ler, conflict'ler ve AtrisBridge baseline'ı olmayan overlap'lar değiştirilmek yerine bloke edilir.

Ayrıntılı model için [docs/backup-engine.md](docs/backup-engine.md) ve [docs/rclone-transport.md](docs/rclone-transport.md) belgelerine bakın.

## Yol haritası

1. **Phase 0/1 — temel mimari ve local inventory** ✅
2. **Phase 2 — SQLite sync journal ve kalıcı dosya state'i** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — güvenli incremental backup** ✅
5. **Phase 5 — pull ve restore**
6. **Phase 6 — conflict-aware two-way sync**
7. **Phase 7 — kalıcı secure credential storage + opsiyonel client-side encryption**
8. **Phase 8+ — sürekli izleme, tray, ek provider'lar ve release pipeline**

Detaylar için [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md), [docs/security.md](docs/security.md), [docs/rclone-transport.md](docs/rclone-transport.md) ve [docs/backup-engine.md](docs/backup-engine.md) belgelerine bakabilirsiniz.

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

## Google Drive davranışı

Google Drive bağlantısı browser tabanlı OAuth ve `drive.file` scope kullanır. Phase 4 write işlemleri ayrıca `AtrisBridge/...` yönetilen namespace ile sınırlandırılmıştır.

OAuth token yalnızca process memory'de tutulur. Uygulama yeniden açıldığında provider'a yeniden bağlanmak gerekir. Secure credential layer gelmeden plaintext token saklamak bilinçli olarak yapılmaz.

Remote provider checksum'ları (örneğin MD5) BLAKE3 ile aynı şeymiş gibi karşılaştırılmaz. Doğrulanmış transfer sonrasında synchronized baseline local BLAKE3 ve provider evidence'ını ayrı alanlarda tutar.

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

Uygulama çalışan bir upload sırasında kapanırsa startup recovery o operation'ı `failed`, planı `partial` olarak emekliye ayırır. Sessizce retry veya synchronized kabulü yapılmaz; sonraki deneme fresh evidence ile yeni plan gerektirir.

## Güvenlik ve şirket politikası

AtrisBridge yanlışlıkla veri sızdırma riskini azaltacak şekilde tasarlanır; ancak şirket veya müşteri kaynak kodunu üçüncü taraf bir servise yüklemek için size yetki vermez. Senkronize ettiğiniz projenin şirket politikalarına, DLP kurallarına, sözleşmelere, data-residency/export-control gereksinimlerine ve yetkilendirme kurallarına her zaman uymalısınız.

Güvenlik açıklarını public issue üzerinden paylaşmayın. [SECURITY.md](SECURITY.md) belgesini kullanın.

## Lisans

[Apache License 2.0](LICENSE) ile lisanslanmıştır.
