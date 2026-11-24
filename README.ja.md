# karukan-skkserv

[English](README.md) | 日本語

[karukan](https://github.com/togatoga/karukan) のかな漢字変換エンジンを使う、skkserv 互換の辞書サーバーです。

SKK クライアントから読みを受け取り、AI モデルの変換結果と karukan のシステム辞書の候補をまとめて返します。エンジンを直接組み込んでいるので、別のサーバーを立てる必要はありません。

## 必要なもの

- Rust 1.92 以降
- C/C++ コンパイラ、CMake、libclang（llama.cpp のビルドに使用）
- 初回のビルドと起動時のネットワーク接続（karukan のソースとモデルを取得するため）

## セットアップ

ビルドします。

```sh
cargo build --release
```

システム辞書を karukan の既定の場所に配置します。

```sh
curl -fLO https://github.com/togatoga/karukan/releases/latest/download/dict.tgz
mkdir -p ~/.local/share/karukan-im
tar xzf dict.tgz -C ~/.local/share/karukan-im dict.bin
```

起動します。

```sh
./target/release/karukan-skkserv
```

既定では `127.0.0.1:1178` で待ち受け、文字コードは EUC-JP です。モデルは初回起動時に Hugging Face から自動でダウンロードされます（既定のモデルで約 110MB）。保存先は `HF_HOME` で変更できます。読み込みが終わるまでは、辞書の候補だけを返します。

辞書とモデルをリポジトリ内の `data/`（Git の管理対象外）に置く場合は、次のように起動します。

```sh
HF_HOME=$PWD/data/hf ./target/release/karukan-skkserv --dict data/dl/dict.bin
```

## クライアントの設定

### ddskk

```elisp
(setq skk-server-host "127.0.0.1"
      skk-server-portnum 1178)
```

### fcitx5-skk

`~/.local/share/fcitx5/skk/dictionary_list` に次の行を追加します。

```text
encoding=EUC-JP,host=127.0.0.1,port=1178,type=server
```

辞書は上から順に引かれます。このサーバーの候補を先に出したい場合は、`user.dict` の行の直後に置いてください。

### libskk の `skk` コマンド

```sh
echo 'K a n j i SPC RET' | skk -s 127.0.0.1:1178 -u /tmp/skk-test.dict
```

動作を確認しているのは libskk の `skk` コマンドだけです。ddskk と fcitx5-skk での実際の入力は未確認です。

## オプション

| オプション               | 既定値                 | 説明                                       |
| ------------------------ | ---------------------- | ------------------------------------------ |
| `--host <HOST>`          | `127.0.0.1`            | 待ち受けるアドレス                         |
| `-p`, `--port <PORT>`    | `1178`                 | 待ち受けるポート                           |
| `--encoding <ENCODING>`  | `euc-jp`               | 通信の文字コード（`euc-jp` または `utf8`） |
| `--dict <DICT>`          | karukan の設定         | システム辞書のパス                         |
| `--model <MODEL>`        | `conversion.model`     | karukan の `[models]` にあるモデルのキー   |
| `--no-model`             |                        | モデルを使わず、辞書だけで応答する         |
| `--model-candidates <N>` | `3`                    | 1 回の検索でモデルから得る候補数           |
| `--max-candidates <N>`   | `30`                   | 1 回の検索で返す候補の上限                 |
| `--timeout-ms <MS>`      | `2000`                 | モデルの変換 1 回の制限時間（ミリ秒）      |
| `--threads <N>`          | `conversion.n_threads` | 推論スレッド数                             |
| `--max-connections <N>`  | `32`                   | 同時接続数の上限                           |

ログの詳細度は環境変数 `RUST_LOG` で変更できます（既定は `info`）。

サーバーに認証や暗号化の機能はありません。`--host` でループバック以外のアドレスを指定する場合は、信頼できるネットワークに限定してください。

### karukan の設定

`~/.config/karukan-im/config.toml` があれば、karukan-im と同じ設定を読み込みます。このサーバーが使う項目は `conversion.model`、`conversion.n_threads`、`conversion.dict_path`、`[models]` です。

辞書のパスは `--dict`、`conversion.dict_path`、`~/.local/share/karukan-im/dict.bin` の順に決まります。辞書を読み込めない場合は、辞書なしで動作します。

## 変換の仕様

- **候補の順序:** モデルの候補、辞書の候補の順に、重複を除いて返します。送りなしの場合は、最後にカタカナも加えます。
- **モデルが使えないとき:** 読み込み中、制限時間の超過、変換エラーのときは、モデルを除いた候補を返します。この結果はキャッシュしません。
- **送りあり:** `かk` は `かく` として変換し、語幹の `書` を返します。
- **対象外のキー:** 英字の略語、`#` を含む数値変換、`>` を含む接頭辞・接尾辞は常に「候補なし」を返します。これらは SKK の個人辞書や他の辞書で引いてください。
- **補完:** skkserv の補完要求（`4`）には、辞書にある読みを返します。
- **文字コード:** EUC-JP で表現できない候補（絵文字など）は応答から除きます。
- **安全対策:** ddskk は `(` で始まる候補を Lisp として評価するため、`(` で始まる候補と `/`、`;` を含む候補は、リテラル文字列の `concat` 式にエスケープします。制御文字を含む候補は返しません。

## 既知の問題

- `hennkann` や `高か` のような不自然な候補が混ざることがあります。
- libskk で送りありの候補を順に送ると、サーバーが返した順序にならないことがあります（`書` の次が `掛` になるなど）。原因は特定できていません。

## 開発

```sh
cargo test
```

karukan は `Cargo.toml` でコミットを固定しています。更新するときは、`karukan-engine` と `karukan-im` の `rev` を揃えて書き換えてから、次のコマンドを実行してください。

```sh
cargo update -p karukan-engine -p karukan-im
```

`.cargo/config.toml` は llama.cpp のビルドで OpenSSL を探さないようにする設定です。依存先の Cargo 設定は適用されないため、このリポジトリで管理しています。

## ライセンス

[MIT](LICENSE)

システム辞書とモデルはこのリポジトリに含まれず、それぞれの配布元のライセンスに従います。karukan のシステム辞書は SudachiDict を元にしており、Apache License 2.0 で配布されています。
