# codextest

Rust 製の PDF 解析ライブラリを PyO3 で Python から利用できるようにしたモジュールです。`pdf` クレートでページの Content Stream を読み取り、テキスト・画像・パスの情報を抽出します。

## 現在実装されている機能

- **ページ数の取得**: `get_page_count(path: &str)` で PDF の総ページ数 (`usize`) を返します。
- **テキスト抽出 (座標付き)**: `extract_text_with_coords(path: &str, page_index: usize)` は、ページ内のテキストを `(文字列, x, y)` のタプルとして返します。`x` と `y` は描画開始位置（ベースライン）の座標です。
- **画像抽出**: `extract_images(path: &str, page_index: usize)` は、ページに埋め込まれた画像を `(バイト列, 幅, 高さ, 形式)` のタプルとして返します。現在は画像の配置座標は取得しておらず、PNG への再エンコードまたは元ストリームをそのまま返します。
- **パス抽出**: `extract_paths(path: &str, page_index: usize)` は、線分・矩形・ベジエ曲線などの描画命令を `(種類, 座標列)` のタプルで返します。座標はページ内で使用されている値をそのまま返します。

## 開発方法

maturin を利用して Python モジュールとして開発・テストできます。

```bash
maturin develop
python -c "import pdfmodule; print(pdfmodule.get_page_count('sample.pdf'))"
```

## Python からの利用例

```python
import pdfmodule

print("Pages:", pdfmodule.get_page_count("sample.pdf"))

texts = pdfmodule.extract_text_with_coords("sample.pdf", 0)
for text, x, y in texts:
    print("TEXT:", text, x, y)

images = pdfmodule.extract_images("sample.pdf", 0)
for data, width, height, fmt in images:
    print("IMAGE:", fmt, width, height, len(data))

paths = pdfmodule.extract_paths("sample.pdf", 0)
for kind, points in paths:
    print("PATH:", kind, points)
```
