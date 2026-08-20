#!/usr/bin/env bash
set -euo pipefail

cache_dir="${ATRISBRIDGE_APT_CACHE_DIR:-$HOME/.cache/atrisbridge/apt-debs}"
mkdir -p "$cache_dir"

if [[ -f /etc/apt/apt-mirrors.txt ]]; then
  sudo sed -i '/azure\.archive\.ubuntu\.com/d' /etc/apt/apt-mirrors.txt
  if ! grep -q 'archive\.ubuntu\.com/ubuntu' /etc/apt/apt-mirrors.txt; then
    echo 'https://archive.ubuntu.com/ubuntu/' | sudo tee -a /etc/apt/apt-mirrors.txt >/dev/null
  fi
fi

for source_file in /etc/apt/sources.list /etc/apt/sources.list.d/ubuntu.sources; do
  if [[ -f "$source_file" ]]; then
    sudo sed -i 's#http://azure\.archive\.ubuntu\.com/ubuntu#https://archive.ubuntu.com/ubuntu#g' "$source_file"
  fi
done

shopt -s nullglob
cached_debs=("$cache_dir"/*.deb)
if ((${#cached_debs[@]} > 0)); then
  sudo cp -f "${cached_debs[@]}" /var/cache/apt/archives/
fi

apt_options=(
  -o Acquire::Retries=2
  -o Acquire::http::Timeout=15
  -o Acquire::https::Timeout=15
  -o Acquire::Languages=none
)

sudo apt-get "${apt_options[@]}" update

packages=(
  libwebkit2gtk-4.1-dev
  build-essential
  curl
  wget
  file
  unzip
  libxdo-dev
  libssl-dev
  libayatana-appindicator3-dev
  librsvg2-dev
  libsecret-1-dev
  libxkbcommon-dev
  patchelf
)

sudo apt-get "${apt_options[@]}" install -y --download-only --no-install-recommends "${packages[@]}"

downloaded_debs=(/var/cache/apt/archives/*.deb)
if ((${#downloaded_debs[@]} > 0)); then
  sudo cp -f "${downloaded_debs[@]}" "$cache_dir"/
  sudo chown -R "$(id -u):$(id -g)" "$cache_dir"
fi

sudo apt-get "${apt_options[@]}" install -y --no-install-recommends "${packages[@]}"
