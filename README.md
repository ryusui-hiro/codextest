# pdfmodule

Rust製のPDF解析機能をPythonから利用できるようにした拡張モジュールです。PDFファイルからテキスト・画像・描画パスなどを抽出し、座標情報付きで扱うことができます。

## Rust製 AI搭載・高精度ベクター変換エンジン (Rust-AI-Vectorizer) 計画と進捗
「Vectorizer.ai」と同等の品質を目指し、AIによる超解像を前処理、`vtracer` によるベクター化を後処理とするCLIツールを段階的に育てていきます。画像品質を落とさずSVG化することをゴールに、フェーズごとにREADMEを更新しながら進行状況を記録します。

### 進捗ログ
- [x] **Phase 1: ベースラインの実装（純粋なベクター変換）** — `vtracer` + `clap` でラスタ画像をSVG化するCLIを実装。イラスト向けにノイズ除去と曲線の滑らかさを優先するプリセットを追加し、`input.jpg` → `output.svg` の流れを確立。
- [x] **Phase 2: AI推論エンジンの統合（前処理）** — `ort` と `Real-ESRGAN/SwinIR` のONNXモデルを読み込み、`image::DynamicImage` と `ndarray::Array4<f32>` の相互変換を実装する。
- [x] **Phase 3: パイプラインの結合とメモリ最適化** — 超解像結果をメモリ上で `vtracer` に渡し、ディスクI/Oを挟まずに高速ベクター化する。大判画像への自動リサイズも盛り込む。
- [ ] **Phase 4: 品質チューニング（High Precision Mode）** — 量子化オプション、前処理フィルター強化、SVGパスのスムージングを追加して「非常に高精度」と言える仕上がりにする。
- [ ] **Phase 5: 配布用パッケージング** — エラーハンドリング強化、進行状況バー導入、`cargo build --release` での配布バイナリ整備。

### これまでに行ったこと（Phase 1）
- `vectorize` CLI を整備し、`vtracer` の主要パラメータ（`colormode`、`hierarchical`、`mode` など）をCLI引数から指定可能にしました。
- イラスト向けのプリセットを新設し、ノイズ除去強め・曲線滑らかめの設定をワンコマンドで適用できるようにしました（詳細は下記CLIの章を参照）。

### 今後の進め方
Phase 2以降は上記のロードマップに沿って、AI超解像（`ort` + ONNXモデル）→オンメモリ連携→品質チューニングの順で機能を追加していきます。各フェーズ完了ごとにREADMEへ進捗を追記し、使い方やパラメータの推奨値も更新予定です。

### Phase 2で追加したもの（AI前処理）
- `ort` を用いた ONNX Runtime のラッパー `SuperResolutionEngine` を実装し、Real-ESRGAN/SwinIR のモデルをそのまま読み込んで推論できるようにしました。
- `image::DynamicImage` と `ndarray::Array4<f32>`（NCHWレイアウト）の相互変換ヘルパーを用意し、RGB/BGRのチャンネル順を指定して0〜1へ正規化・復元できます。
- Real-ESRGAN/SwinIR の公開ONNXモデルをダウンロードする CLI `download_models` を追加しました（既存ファイルはスキップ）。

### Phase 3で追加したもの（オンメモリ結合 + 自動リサイズ）
- `vectorize` CLI に超解像ONNXモデルを指定する `--superres-model` オプションを追加し、`image -> tensor -> image -> SVG` の流れを全てメモリ上で処理するようにしました。推論結果の画像は `vtracer` の `ColorImage` に直接変換し、ディスクI/Oを挟みません。
- Real-ESRGANなどで一般的なBGR入力に合わせてチャンネル順を切り替える `--channel-order` を新設しました（デフォルトBGR）。
- メモリ節約と推論速度のため、入力/出力の最大辺を `--max-dimension` で指定し、大判画像は自動リサイズした上で超解像・ベクター化します（デフォルト4096px）。

#### 変換・推論の使い方（Rust）
```rust
use image::open;
use pdfmodule::ai::{
    dynamic_image_to_nchw_f32, nchw_f32_to_dynamic_image, ChannelOrder,
    SuperResolutionEngine, REALESRGAN_X4PLUS_ONNX,
};

let input = open("input.png")?;
let tensor = dynamic_image_to_nchw_f32(&input, ChannelOrder::Bgr);
let engine = SuperResolutionEngine::from_onnx("models/realesrgan-x4plus.onnx")?;
let output_tensor = engine.run(&tensor)?;
let upscaled = nchw_f32_to_dynamic_image(&output_tensor, ChannelOrder::Bgr)?;
upscaled.save("upscaled.png")?;
```

#### ONNXモデルのダウンロード
```bash
cargo run --bin download_models -- --output-dir models
```
`models/realesrgan-x4plus.onnx` と `models/swinir_x4.onnx` が保存されます。

## 機能
- **ページ数の取得**: `get_page_count(path: str) -> int` がPDFの総ページ数を返します。
- **テキスト抽出**: `extract_text_with_coords(path: str, page_index: int)` は指定ページのテキストを行ごとの辞書リストで返します。各辞書には `type`, `text`, `x`, `y`, `x0`, `y0`, `x1`, `y1` が含まれ、ベースライン座標とバウンディングボックスを確認できます。
- **画像抽出**: `extract_images(path: str, page_index: int)` はページ内の画像を辞書として返します。返却値には `type`, `name`, `x0`〜`y1` の座標、`width`, `height`, `format`, `data` などが含まれ、`data` はPNGなどのバイナリデータです。
- **描画パス抽出**: `extract_paths(path: str, page_index: int)` は線分・曲線・矩形などの描画命令を `(kind, points)` のタプルで返します。`kind` は `line`・`curve`・`rect` など、`points` は座標のリストです。
- **ページ内容の統合取得**: `extract_page_content(path: str, page_index: int)` はページ内のテキスト・画像・パスをまとめた辞書を返します。`text`/`images`/`objects` に加えて、検出したレイアウトをまとめた `layouts` とページ内の項目を順序通り格納した `items` リストを持ちます。
- **レイアウト抽出**: `extract_layouts(path: str, page_index: int, text_color=None, image_color=None, object_color=None)` はテキスト群、画像、描画オブジェクトをレイアウト単位にまとめた辞書リストを返します。各レイアウトには矩形領域(`x0`〜`y1`)と色指定(`color`)、図表タイトルなどが見つかった場合は `captions` が含まれます。
- **任意座標の矩形生成**: `make_rectangle_outline(x0, y0, x1, y1, color=None)` は任意の座標範囲に対する矩形枠を辞書として生成し、独自の描画処理に利用できます。


> **ページ番号について**: `page_index` は0始まり（最初のページは0）です。

## インストール
このプロジェクトは [maturin](https://github.com/PyO3/maturin) を利用してビルドします。Python 3.8以上とRustツールチェーンが必要です。

```bash
# 依存ツールの導入
pip install maturin

# 開発環境へインストール（仮想環境推奨）
maturin develop --release

# またはwheelをビルドしてからインストール
maturin build --release
pip install target/wheels/pdfmodule-*.whl
```

## Pythonでの利用例
```python
import pdfmodule

pdf_path = "sample.pdf"

# 総ページ数
total_pages = pdfmodule.get_page_count(pdf_path)
print("pages:", total_pages)

# 1ページ目のテキスト（0始まり）
text_items = pdfmodule.extract_text_with_coords(pdf_path, 0)
for item in text_items:
    print(item["text"], item["x0"], item["y0"], item["x1"], item["y1"])

# 画像抽出と保存
for image in pdfmodule.extract_images(pdf_path, 0):
    ext = image["format"]  # 例: "png", "jpeg", "raw" など
    with open(f"{image['name']}.{ext}", "wb") as fh:
        fh.write(image["data"])

# 描画パスの取得
path_segments = pdfmodule.extract_paths(pdf_path, 0)
for kind, points in path_segments:
    print(kind, points)

# ページ内容をまとめて取得
page_summary = pdfmodule.extract_page_content(pdf_path, 0)
print(page_summary["page_index"], len(page_summary["items"]))
# レイアウトの矩形情報を取得
layouts = pdfmodule.extract_layouts(pdf_path, 0)
for layout in layouts:
    print(layout["type"], layout["x0"], layout["y0"], layout["color"])
    for caption in layout.get("captions", []):
        print("  caption:", caption["text"])
```

それぞれの戻り値は標準的なPythonのデータ構造（辞書、リスト、タプル、バイト列）なので、pandasやPillowなどの外部ライブラリと組み合わせて自在に処理できます。

## 画像をSVGへベクター化するCLI
`cargo install --path .` などでバイナリをビルドすると、`vectorize` コマンドが利用できます。ラスタ画像をvtracerでトレースしてSVGを生成します。

```bash
# 入力画像を同名の .svg に変換
vectorize input.png

# 出力パスやモードを指定（イラスト向けプリセットがデフォルト）
vectorize input.jpg --output output.svg --color-mode binary --hierarchy cutout --mode polygon

# 写真向けにプリセットを切り替え、色数を上書き
vectorize input.png --preset natural --colors 24

# パスの最適化パラメータも上書き可能
vectorize input.png --filter-speckle 2 --corner-threshold 80 --path-precision 2 --round-coords true
```

主なオプション:
- `--color-mode [color|binary]`: カラーモード指定。
- `--hierarchy [stacked|cutout]`: パスの積層方法。
- `--mode [spline|polygon]`: スプラインかポリゴンか。
- `--preset [illustration|natural]`: 利用シーンに合わせたプリセット（デフォルトは `illustration`）。
- `--colors <N>`: 量子化する色数（プリセットを上書きしたい場合に指定）。
- `--filter-speckle <N>`: 小さなゴミを除去するピクセル閾値。
- `--corner-threshold <角度>` / `--length-threshold <長さ>` / `--splice-threshold <値>`: パス簡略化のしきい値。
- `--max-iterations <N>`: ベジェ近似の最大繰り返し回数。
- `--path-precision <桁数>` / `--round-coords <true|false>`: SVG座標の精度や丸め設定。
- `--optimize-paths <true|false>`: パス最適化のオン/オフ。
- `--superres-model <path>`: 超解像ONNXモデルを指定し、オンメモリで前処理してからベクター化。
- `--channel-order [rgb|bgr]`: モデルが期待するチャンネル順を切り替え（デフォルトBGR）。
- `--max-dimension <px>`: 入力/出力画像の最大辺。超過する場合は自動リサイズしてメモリ使用量を抑制。
