# pdfmodule

Rust製のPDF解析機能をPythonから利用できるようにした拡張モジュールです。PDFファイルからテキスト・画像・描画パスなどを抽出し、座標情報付きで扱うことができます。

## 機能
- **ページ数の取得**: `get_page_count(path: str) -> int` がPDFの総ページ数を返します。
- **テキスト抽出**: `extract_text_with_coords(path: str, page_index: int)` は指定ページのテキストを行ごとの辞書リストで返します。各辞書には `type`, `text`, `x`, `y`, `x0`, `y0`, `x1`, `y1` が含まれ、ベースライン座標とバウンディングボックスを確認できます。
- **画像抽出**: `extract_images(path: str, page_index: int)` はページ内の画像を辞書として返します。返却値には `type`, `name`, `x0`〜`y1` の座標、`width`, `height`, `format`, `data` などが含まれ、`data` はPNGなどのバイナリデータです。
- **描画パス抽出**: `extract_paths(path: str, page_index: int)` は線分・曲線・矩形などの描画命令を `(kind, points)` のタプルで返します。`kind` は `line`・`curve`・`rect` など、`points` は座標のリストです。
- **ページ内容の統合取得**: `extract_page_content(path: str, page_index: int)` はページ内のテキスト・画像・パスをまとめた辞書を返します。`text`/`images`/`objects` に加えて、ページ内の項目を順序通り格納した `items` リストを持ちます。

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
```

それぞれの戻り値は標準的なPythonのデータ構造（辞書、リスト、タプル、バイト列）なので、pandasやPillowなどの外部ライブラリと組み合わせて自在に処理できます。
