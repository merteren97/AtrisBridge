# AtrisBridge

AtrisBridge; yazılım ve mühendislik projelerini farklı bilgisayarlar arasında daha güvenli, takip edilebilir ve taşınabilir hale getirmek için geliştirilen local-first bir masaüstü uygulamasıdır.

> **Durum:** erken alpha (`0.1.0-alpha.1`). Mevcut sürüm yalnızca yerel workspace envanteri çıkarır. Cloud transport ve iki yönlü senkronizasyon bilinçli olarak henüz aktif değildir.

[English README](README.md)

## Neden AtrisBridge?

Aktif projeleri geliştirme bilgisayarları arasında taşımak çoğu zaman ZIP dosyaları, manuel cloud klasörleri, eski kopyalar ve hangi sürümün güncel olduğuna dair belirsizlik oluşturur. AtrisBridge yerel proje ile depolama sağlayıcısı arasına güvenli bir katman koymak için tasarlanır:

- local-first workspace yönetimi,
- kaynak kod ve secret dosyalar için korumacı varsayılan ignore kuralları,
- `.atrisbridgeignore` desteği,
- BLAKE3 içerik fingerprint'leri,
- tarama sırasında symlink takip etmeme,
- "son değişen kazanır" yerine ileride conflict-aware senkronizasyon,
- Google Drive ilk hedef olmak üzere provider-independent mimari.

## Şu anda çalışan özellikler

Phase 0 ve Phase 1 güvenlik ve workspace temelini oluşturur:

- Tauri 2 + React + TypeScript masaüstü shell,
- yerel workspace ekleme ve kaldırma,
- workspace metadata'sını işletim sisteminin application-data dizininde saklama,
- native klasör seçici,
- Rust workspace scanner,
- BLAKE3 dosya fingerprint'leri,
- build çıktıları, Git metadata'sı, IDE cache'leri, environment dosyaları ve yaygın key/certificate formatları için built-in exclude kuralları,
- isteğe bağlı `.atrisbridgeignore` oluşturma,
- scan özeti ve dosya önizleme arayüzü,
- frontend ve Rust doğrulaması için Linux CI.

AtrisBridge üzerinden bir workspace'i kaldırmak gerçek proje dosyalarını silmez.

## Yol haritası

1. **Phase 0/1 — temel mimari ve local inventory** ✅
2. **Phase 2 — SQLite sync journal ve kalıcı dosya state'i**
3. **Phase 3 — rclone sidecar ve Google Drive provider**
4. **Phase 4 — güvenli incremental backup**
5. **Phase 5 — pull ve restore**
6. **Phase 6 — conflict-aware two-way sync**
7. **Phase 7 — client-side encryption ve güvenli credential saklama**
8. **Phase 8+ — sürekli izleme, tray, ek provider'lar ve release pipeline**

Detaylar için [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md) ve [docs/security.md](docs/security.md) belgelerine bakabilirsiniz.

## Geliştirme

### Gereksinimler

- Node.js LTS
- npm
- Rust stable
- İşletim sisteminiz için Tauri 2 gereksinimleri

### Çalıştırma

```bash
npm install
npm run tauri:dev
```

## `.atrisbridgeignore`

Workspace kökünde `.atrisbridgeignore` adlı bir dosya kullanarak gitignore uyumlu proje kuralları ekleyebilirsiniz. Custom dosya bulunmasa bile AtrisBridge'in built-in güvenlik kuralları çalışmaya devam eder.

```gitignore
artifacts/
cache/
customer-dumps/
*.bak
```

## Güvenlik ve şirket politikası

AtrisBridge yanlışlıkla veri sızdırma riskini azaltacak şekilde tasarlanır; ancak şirket veya müşteri kaynak kodunu üçüncü taraf bir servise yüklemek için size yetki vermez. Senkronize ettiğiniz projenin şirket politikalarına ve yetkilendirme kurallarına her zaman uymalısınız.

Güvenlik açıklarını public issue üzerinden paylaşmayın. [SECURITY.md](SECURITY.md) belgesini kullanın.

## Lisans

[Apache License 2.0](LICENSE) ile lisanslanmıştır.
