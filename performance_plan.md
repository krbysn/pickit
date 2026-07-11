# 巨大リポジトリにおける初期表示高速化の検討・計画書

本ドキュメントは、巨大なGitリポジトリで `pickit` を起動した際に初期表示（TUI画面の描画開始）までに時間がかかる問題の原因を分析し、高速化を行うための実装計画をまとめたものです。

---

## 1. 現状のボトルネック分析

起動処理を調査した結果、初期化処理における以下の同期的な処理がボトルネックになっていることが判明しました。

### 起動時の同期的かつ再帰的なディレクトリ走査

1. **`App::new`**: `App` 構造体の生成時に、メインスレッド上で `self.load_initial_tree()` を同期的に呼び出します。
2. **`load_initial_tree`**: リポジトリルート直下のディレクトリ一覧を取得するため、以下の関数を呼び出します。
   ```rust
   let top_level_dirs = git::get_dirs_at_path(".", &self.current_repo_root)?;
   ```
3. **`git::get_dirs_at_path`**:
   - ディレクトリが物理的にチェックアウトされて存在する場合（起動時のルートディレクトリなど）、以下のコマンドを実行します（Strategy 1）。
     ```bash
     git ls-tree -r --name-only -d HEAD
     ```
   - ディレクトリが仮想（非チェックアウト）状態の場合、以下のコマンドを実行します（Strategy 2）。
     ```bash
     git ls-tree -r --name-only -d HEAD
     ```
   - ここで **`-r`（再帰的）フラグ** が指定されているため、いずれの場合も**リポジトリ内のすべてのディレクトリ（数万件〜数十万件）を再帰的に走査**します。
   - その後、Rust側で「スラッシュを含まないもの（直近の子のみ）」や「特定のプレフィックスで始まるもの」をフィルタリングして捨てています。

### 巨大リポジトリでの影響
数十万ディレクトリを抱える巨大なモノレポなどでは、以下の影響が発生します。
- `git ls-tree -r` の実行自体に数秒〜数十秒（ディスクによっては分単位）かかります。
- 大量のパス文字列をパース・フィルタリングするため、メモリとCPUを消費します。
- これらすべてが起動時に同期的に（UIを描画する前に）実行されるため、画面が表示されるまでフリーズしたようになります。

---

## 2. 提案する最適化: 非再帰ツリークエリの採用

この問題を解決するため、再帰的に全ディレクトリを走査するのではなく、**指定されたディレクトリの直下（1レベル）のみ**を非再帰的かつ高速に取得するように変更します。

### 非再帰的な `git ls-tree`
`-r` フラグを排除し、対象パスのみを直接クエリします。
- **ルート（`"."` または `""`）の場合**:
  ```bash
  git ls-tree -d --name-only HEAD
  ```
  *ルート直下のディレクトリ（例: `src`, `docs`）のみを即座に返します。*

- **サブディレクトリ（例: `"src"`）の場合**:
  ```bash
  git ls-tree -d --name-only HEAD src/
  ```
  *`src/` の直下にあるディレクトリ（例: `src/components`）のみを返します。末尾にスラッシュを付与することで、その配下の中身をリストします。*

### 最適化による効果
1. **リポジトリ規模に依存しない高速化 (O(1))**: `-r` を指定しない `git ls-tree` は、Gitデータベースから単一のツリーオブジェクト（そのディレクトリのメタデータ）を読み出すだけであるため、リポジトリの総サイズや深さに関わらず **数ミリ秒（ほぼ瞬時）** で完了します。
2. **処理の共通化**: `git ls-tree` は実ファイルではなくGitインデックス（`HEAD` コミット）を参照するため、対象ディレクトリが物理的に存在するか（チェックアウト済）、仮想状態であるかに関わらず全く同じように動作します。そのため、これまでの「Strategy 1（物理）」と「Strategy 2（仮想）」の条件分岐や、Rust側での重い全走査フィルタリング処理をすべて撤廃し、シンプルな実装に統合できます。
3. **無駄な文字列処理の削減**: コマンドの出力には直下の子ディレクトリのみが含まれるため、Rust側では親ディレクトリのプレフィックス（例: `src/`）を取り除くだけで、単純なディレクトリ名を取得できます。

---

## 3. 具体的な実装計画

`src/git.rs` 内の `git::get_dirs_at_path` 関数を以下のように更新します。

### 変更予定のコード (`src/git.rs`)

```rust
pub fn get_dirs_at_path(path: &str, repo_path: &Path) -> Result<Vec<String>> {
    let mut args = vec!["ls-tree", "-d", "--name-only", "HEAD"];
    
    // サブディレクトリが指定されている場合、その直下のコンテンツのみをクエリする
    let path_arg;
    if !path.is_empty() && path != "." {
        path_arg = if path.ends_with('/') {
            path.to_string()
        } else {
            format!("{}/", path)
        };
        args.push(&path_arg);
    }

    let output = run_git_command(&args, Some(repo_path))?;
    let lines = parse_path_lines(output)?;
    
    let mut direct_children = Vec::new();
    for line in lines {
        // 親ディレクトリのプレフィックスを除去して、単純なディレクトリ名のみを抽出する
        let simple_name = if !path.is_empty() && path != "." {
            let prefix = if path.ends_with('/') {
                path.to_string()
            } else {
                format!("{}/", path)
            };
            if line.starts_with(&prefix) {
                line[prefix.len()..].to_string()
            } else {
                // フォールバック: 最後のスラッシュ以降の部分を取得
                if let Some(idx) = line.rfind('/') {
                    line[idx + 1..].to_string()
                } else {
                    line
                }
            }
        } else {
            line
        };
        
        if !simple_name.is_empty() {
            direct_children.push(simple_name);
        }
    }

    Ok(direct_children)
}
```

### 検証とテスト計画
1. `cargo test` を実行し、既存のテスト（特に仮想ディレクトリの展開テスト `test_expand_non_checked_out_directory` や `test_get_dirs_at_path`）が問題なくパスすることを確認します。
2. テストの実行時間や、実際の動作での体感速度が向上することを確認します。
