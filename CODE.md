# CODE specification

このドキュメントは本リポジトリの「コードが何をどのように実装しているか」をまとめた仕様書です。CLI や Python API の使い方は README を参照してください。

## 構成概要
- **クレート名 / Python モジュール名**: `pdfvectorizer`
- **バイナリ**: `vectorize`（画像ベクター化 CLI）、`download_models`（ONNX モデル取得）
- **ライブラリ**: PyO3 ベースの PDF 解析 API を提供。
- **主な依存**: `vtracer` で SVG 化、`image` で前処理、`ort` で ONNX 推論、`nipdf` / `pdf` / `nipdf-render` で PDF 解析。

## 画像ベクター化 CLI (`src/bin/vectorize.rs`)
- **パイプライン**
  1. `image` crate でラスタ画像を読み込み、`Args` で指定されたプリセットに応じて `vtracer::Config` を初期化。
  2. `QualityPreset` / `PresetChoice` でノイズ除去・アンシャープマスク・平滑化閾値を一括設定。
  3. `SuperResolutionEngine`（`ai.rs`）を介して ONNX 超解像を実行。`ChannelOrder` で RGB/BGR を切り替え、`max_dimension` でメモリを抑制。
  4. `visioncortex::CompoundPath` を用いてパス平滑化。`SmoothingSettings` で角閾値・外周オフセット率・セグメント長を調整。
  5. `vtracer::conversion::convert_image_to_svg` で SVG を生成し、`SvgFile` としてディスクへ保存。
- **エラーモデル**: `VectorizeError` が I/O、モデルロード、ONNX 推論結果の変換、SVG 出力失敗を区別して `thiserror` で整形。
- **進行ログ**: `StepLogger` で処理ステップ（ロード、前処理、推論、変換、保存）を逐次表示し、失敗時に最後のステップを明示。

## モデルダウンロード CLI (`src/bin/download_models.rs`)
- **役割**: Real-ESRGAN と SwinIR の ONNX モデルを所定ディレクトリへ保存。既存ファイルは `skip_existing` オプションでスキップ。
- **実装**: `download_model_with_progress`（`ai.rs`）によりストリーミングダウンロードし、`indicatif` のプログレスバーで転送量を可視化。
- **エラー処理**: HTTP エラー、書き込み失敗、レスポンス異常を `AiError` にラップして CLI へ伝搬。

## AI ヘルパー (`src/ai.rs`)
- **ONNX 推論**: `SuperResolutionEngine` が ONNX Runtime (`ort`) を初期化し、入力画像を NCHW f32 へ前処理。`dynamic_image_to_nchw_f32` / `nchw_f32_to_dynamic_image` で画像↔テンソル変換。
- **モデル入出力**: `InferenceInput` でチャネル順序・リサイズ後の寸法を保持し、推論後に元の画像サイズへクロップ。
- **ダウンロード関数**: `download_model_with_progress` が HTTP リクエストを逐次読み込み、`DownloadBar` コールバックで進捗を報告。`download_models_with_progress` は複数 URL をまとめて処理。

## Python 拡張 (`src/lib.rs`)
- **モジュール名**: `pdfvectorizer`（`#[pymodule]`）。
- **提供関数（抜粋）**:
  - `get_page_count(path)` ページ数を返す。
  - `extract_text_with_coords(path, page)` 文字列と座標矩形を抽出。
  - `extract_images(path, page)` / `extract_region_images(path, page, x0, y0, x1, y1)` で画像を PNG バイト列として返却。
  - `extract_paths(path, page)` PDF の描画パスを座標とスタイル情報付きで返す。
  - `extract_layouts(path, page)` テキスト・画像・パスを 1 つの辞書にまとめた高レベル API。
  - `extract_page_content(path, page)` 低レベルに近い生データを返却。
- **PDF 解析**: `pdf` crate でコンテンツストリームを走査し、文字描画オペレーター (`Op::TJ` 等) を解析。`nipdf-render` で画像抽出時のバイナリ生成を行う。
- **座標処理**: `Matrix` を用いたテキスト座標変換、`ResolvedFont` でフォント幅・ToUnicode マップを解決し、`decode_cid` / `decode_simple` でテキストを UTF-8 へ復号。
- **エラー変換**: `PdfError` / `ObjectValueError` を Python の `PyRuntimeError` に変換し、原因を文字列として伝搬。

## ロギングとプログレスバー
- **StepLogger**: CLI 向けの簡易ステップトラッカーで、成功/失敗を明示的な文言で表示。
- **DownloadBar**: モデル取得時に合計バイト数と現在値を追跡し、`indicatif` でプログレスバーを描画。

## ビルド成果物
- `cargo build --release`: `target/release/vectorize`, `target/release/download_models`, `target/release/libpdfvectorizer.so`（OS に応じた拡張子）を生成。
- `maturin build --release`: `target/wheels/pdfvectorizer-*.whl` が生成され、Python から `import pdfvectorizer` で利用可能。
