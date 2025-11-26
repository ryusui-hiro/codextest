# pdfvectorizer

Rust 製の PDF 解析・画像ベクター化ツールキットです。Python 拡張としてのライブラリと、配布可能な CLI バイナリ群（`vectorize` / `download_models`）を同じリポジトリで提供します。Phase 5 ではエラーハンドリングと進行状況の可視化を強化し、`cargo build --release` でそのまま配布できる形に仕上げました。コード実装の仕様は [CODE.md](./CODE.md) にまとめています。

## 主な機能
- **PDF 解析（PyO3 拡張）**: ページ数の取得、テキスト/画像/描画パス抽出、レイアウト情報取得などを Python から呼び出せます。
- **画像ベクター化 CLI (`vectorize`)**: ラスタ画像を前処理（超解像 + フィルター）→ `vtracer` で SVG 化。プリセットと詳細パラメータを CLI で切り替え可能。
- **ONNX モデルダウンロード CLI (`download_models`)**: Real-ESRGAN / SwinIR の ONNX モデルを取得。進捗バー付きで大きなファイルも安心。
- **進行状況バーと詳細なエラー**: 長い処理やネットワーク転送にスピナー/プログレスバーを表示し、失敗時は原因を明示したメッセージで終了します。

## クイックスタート
単純に「画像を SVG にする」最短ルート:

```bash
cargo build --release
./target/release/vectorize input.png  # => input.svg が生成される
```

Python モジュール経由で PDF を扱う最小例:

```bash
pip install maturin
maturin develop --release  # ローカル環境にインストール
```

```python
import pdfvectorizer
print(pdfvectorizer.get_page_count("sample.pdf"))
```

## ビルドと配布
リポジトリ直下でリリースビルドを実行すると、配布に使えるバイナリが `target/release/` に生成されます。

```bash
# 依存関係をダウンロードしつつリリースビルド
cargo build --release

# 生成物の例
ls target/release
# -> vectorize  download_models  libpdfvectorizer.so  ...
```

Python 向けホイールを作る場合は [maturin](https://github.com/PyO3/maturin) を利用してください。

```bash
pip install maturin
maturin build --release
pip install target/wheels/pdfvectorizer-*.whl
```

## CLI の使い方
### 1. モデルを準備する (`download_models`)
ONNX モデルは事前にダウンロードしておくとオフラインで動かせます。既に存在する場合はスキップされます。

```bash
# 標準の models ディレクトリへ保存（進捗バー表示）
./target/release/download_models

# 保存先を変える場合
./target/release/download_models --output-dir assets/models
```

### 2. 画像を SVG に変換する (`vectorize`)
イラスト向けプリセットがデフォルト。大きな画像は自動でリサイズし、指定すれば超解像モデルを前処理として動かします。詳細オプションや追加例は [CODE.md](./CODE.md) を参照してください。

## Python API の概要
Python からは `pdfvectorizer` を import して PDF のページ情報・テキスト・画像・描画パスを取得できます。実装の詳細構成は [CODE.md](./CODE.md) に記載しています。

画像ベクター化も Python から簡単に利用できます。ファイルパスまたは画像バイト列を渡せば SVG 文字列を返し、必要に応じてファイルへ保存できます。

```python
import pdfvectorizer

# 画像パスを渡して SVG 文字列を取得（自動で大きすぎる画像を縮小）
svg = pdfvectorizer.vectorize_image("input.png", output_path="output.svg")

# メモリ上の画像バイト列から SVG を取得する場合
with open("input.jpg", "rb") as f:
    raw = f.read()
svg_bytes = pdfvectorizer.vectorize_image_bytes(raw)
with open("inline.svg", "wb") as f:
    f.write(svg_bytes)
```

## テスト・動作確認
- Rust コードのビルド確認: `cargo build --release`
- Python ホイールの生成確認（任意）: `maturin build --release`

大規模な依存関係を含むため初回ビルドは時間がかかりますが、以降はキャッシュが効いて高速に再ビルドできます。

### ビルド時のよくあるエラー
- `failed to download from https://index.crates.io/config.json` など crates.io への接続 403/timeout: ネットワークやプロキシで crates.io への HTTPS アクセスが遮断されている可能性があります。社内プロキシ経由での TLS トンネル拒否が多いケースです。環境変数 `HTTPS_PROXY` を設定して認証付きプロキシを経由する、VPN を利用して外部ネットワークに出る、あるいは crates.io ミラー（`CARGO_REGISTRIES_CRATES_IO_PROTOCOL=git` など）を利用してください。
