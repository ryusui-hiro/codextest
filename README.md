# pdfmodule

Rust製のPDF解析機能をPythonから利用できるようにした拡張モジュールです。PDFファイルからテキスト・画像・描画パスなどを抽出し、座標情報付きで扱うことができます。

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

# 出力パスやモードを指定
vectorize input.jpg --output output.svg --color-mode binary --hierarchy cutout --mode polygon --colors 8

# パスの最適化パラメータも上書き可能
vectorize input.png --filter-speckle 2 --corner-threshold 80 --path-precision 2 --round-coords true
```

主なオプション:
- `--color-mode [color|binary]`: カラーモード指定。
- `--hierarchy [stacked|cutout]`: パスの積層方法。
- `--mode [spline|polygon]`: スプラインかポリゴンか。
- `--colors <N>`: 量子化する色数。
- `--filter-speckle <N>`: 小さなゴミを除去するピクセル閾値。
- `--corner-threshold <角度>` / `--length-threshold <長さ>` / `--splice-threshold <値>`: パス簡略化のしきい値。
- `--max-iterations <N>`: ベジェ近似の最大繰り返し回数。
- `--path-precision <桁数>` / `--round-coords <true|false>`: SVG座標の精度や丸め設定。
- `--optimize-paths <true|false>`: パス最適化のオン/オフ。
