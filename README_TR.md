# AtrisBridge

AtrisBridge; yazılım ve mühendislik projelerini farklı bilgisayarlar arasında daha güvenli, takip edilebilir ve taşınabilir hale getirmek için geliştirilen local-first bir masaüstü uygulamasıdır.

> **Durum:** erken alpha (`0.1.0-alpha.2`). Yerel envanter, kalıcı SQLite sync journal ve ilk Google Drive transport-observation katmanı uygulanmıştır. Dosya transferi ve destructive sync bilinçli olarak hâlâ kapalıdır.

[English README](README.md)

## Neden AtrisBridge?

Aktif projeleri geliştirme bilgisayarları arasında taşımak çoğu zaman ZIP dosyaları, manuel cloud klasörleri, eski kopyalar ve hangi sürümün güncel olduğuna dair belirsizlik oluşturur. AtrisBridge yerel proje ile depolama sağlayıcısı arasına güvenli bir katman koymak için tasarlanır:

- local-first workspace yönetimi,
- uygulama yeniden başlatıldığında kaybolmayan SQLite dosya state'i,
- kaynak kod ve secret dosyalar için korumacı varsayılan ignore kuralları,
- `.atrisbridgeignore` desteği,
- BLAKE3 içerik fingerprint'leri,
- tarama sırasında symlink takip etmeme,
- destructive sync açılmadan önce tasarlanmış conflict ve tombstone state'leri,
- ilk provider olarak Google Drive kullanan provider-independent transport mimarisi.

## Şu anda çalışan özellikler

Phase 0'dan Phase 3'e kadar local state ve cloud observation temeli tamamlandı:

- Tauri 2 + React + TypeScript masaüstü shell,
- yerel workspace ekleme/kaldırma,
- Rust workspace scanner ve BLAKE3 fingerprint'leri,
- güvenli built-in exclude kuralları ve `.atrisbridgeignore`,
- application-data altında SQLite veritabanı,
- eski `workspaces.json` için tek seferlik migration,
- kalıcı local scan geçmişi ve tam dosya envanteri,
- local/remote/last-synced observation modeli,
- restart-safe state ve recoverable tombstone altyapısı,
- gelecekteki transferler için pending-operation şeması,
- tam olarak `v1.74.4` isteyen doğrulanmış/pinned rclone runtime katmanı,
- least-privilege `drive.file` scope ile Google Drive OAuth,
- Phase 3'te OAuth token'ını yalnızca process memory'de tutma; SQLite veya `rclone.conf` içine token yazmama,
- workspace → Google Drive klasörü mapping'i,
- `rclone lsjson` ile read-only remote inventory,
- Google Drive ID, timestamp ve provider checksum'larını BLAKE3'ten ayrı saklama,
- bilinen sync baseline'ı olmadan local/remote aynı path'e denk gelirse güvenli conflict state'i,
- frontend ve Rust doğrulaması için Linux CI.

Workspace kaldırmak yalnızca AtrisBridge metadata'sını siler. Gerçek proje dizini silinmez. Provider'ı unutmak da Google Drive'daki hiçbir veriyi silmez.

## Yol haritası

1. **Phase 0/1 — temel mimari ve local inventory** ✅
2. **Phase 2 — SQLite sync journal ve kalıcı dosya state'i** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — güvenli incremental backup**
5. **Phase 5 — pull ve restore**
6. **Phase 6 — conflict-aware two-way sync**
7. **Phase 7 — kalıcı secure credential storage + opsiyonel client-side encryption**
8. **Phase 8+ — sürekli izleme, tray, ek provider'lar ve release pipeline**

Detaylar için [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md), [docs/security.md](docs/security.md) ve [docs/rclone-transport.md](docs/rclone-transport.md) belgelerine bakabilirsiniz.

## Geliştirme

### Gereksinimler

- Node.js LTS
- npm
- Rust stable
- İşletim sisteminiz için Tauri 2 gereksinimleri

### Pinned rclone development sidecar'ını hazırlama

AtrisBridge sistem `PATH`'inden rastgele bir `rclone` executable çalıştırmaz. Local geliştirmede önce doğrulanmış binary'yi hazırlayın:

```bash
npm install
npm run sidecar:prepare
npm run tauri:dev
```

`sidecar:prepare`, resmi rclone release host'undan `v1.74.4` arşivini indirir, platforma ait SHA-256 değerini doğrular ve executable'ı `src-tauri/binaries/` altına yerleştirir. Binary Git tarafından ignore edilir.

## Phase 3 Google Drive davranışı

Bu aşamada yalnızca **observation** vardır. Google Drive bağlantısı browser tabanlı OAuth flow açar ve `drive.file` scope ister. Bu scope ile AtrisBridge/rclone yalnızca aynı OAuth uygulaması tarafından oluşturulan dosya ve klasörleri görebilir; Drive'daki alakasız veriler için geniş erişim istenmez.

OAuth token yalnızca process memory'de tutulur. Uygulama yeniden açıldığında provider'a yeniden bağlanmak gerekir. Secure credential layer gelmeden plaintext token saklamak yerine bu kısıt bilinçli olarak kabul edilmiştir.

Remote inventory provider-native MD5 gibi checksum'ları observation olarak saklar. Bunlar doğrudan BLAKE3 ile karşılaştırılmaz. Gerçek cross-provider sync baseline ancak Phase 4 ve sonrasında AtrisBridge'in doğruladığı bir transfer sonrasında kurulacaktır.

## `.atrisbridgeignore`

Workspace kökünde `.atrisbridgeignore` adlı bir dosya kullanarak gitignore uyumlu proje kuralları ekleyebilirsiniz. Custom dosya bulunmasa bile AtrisBridge'in built-in güvenlik kuralları çalışmaya devam eder.

```gitignore
artifacts/
cache/
customer-dumps/
*.bak
```

## Kalıcı sync journal

AtrisBridge application state'ini işletim sisteminin application-data dizinindeki `atrisbridge.db` dosyasında saklar. Local ve remote observation'lar ayrı tutulur; transport katmanı yalnızca modification time'a bakarak karar vermez.

Yerel bir dosyanın kaybolması otomatik olarak remote delete yetkisi anlamına gelmez. Yalnızca bilinen bir synchronized baseline bulunan dosyalar tombstone olabilir; ilerideki transport katmanı provider Trash işlemi öncesinde remote state'i yeniden doğrulamak zorundadır.

## Güvenlik ve şirket politikası

AtrisBridge yanlışlıkla veri sızdırma riskini azaltacak şekilde tasarlanır; ancak şirket veya müşteri kaynak kodunu üçüncü taraf bir servise yüklemek için size yetki vermez. Senkronize ettiğiniz projenin şirket politikalarına ve yetkilendirme kurallarına her zaman uymalısınız.

Güvenlik açıklarını public issue üzerinden paylaşmayın. [SECURITY.md](SECURITY.md) belgesini kullanın.

## Lisans

[Apache License 2.0](LICENSE) ile lisanslanmıştır.
