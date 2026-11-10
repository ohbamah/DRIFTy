//! `+ajoutées −retirées` d'une modification de fichier (Edit, MultiEdit, Write),
//! calculé depuis l'entrée de l'outil. Un seul calcul, pour le fil d'une
//! conversation passée comme pour un chat en direct.

use serde_json::Value;

/// Lignes ajoutées et retirées entre deux textes : on ôte les lignes communes
/// de début et de fin, le reste compte (comme le résumé d'un Edit dans Claude Code).
pub fn lines(old: &str, new: &str) -> (i64, i64) {
    let a: Vec<&str> = if old.is_empty() { vec![] } else { old.lines().collect() };
    let b: Vec<&str> = if new.is_empty() { vec![] } else { new.lines().collect() };
    let mut start = 0;
    while start < a.len() && start < b.len() && a[start] == b[start] {
        start += 1;
    }
    let mut end = 0;
    while end < a.len() - start && end < b.len() - start && a[a.len() - 1 - end] == b[b.len() - 1 - end] {
        end += 1;
    }
    ((b.len() - start - end) as i64, (a.len() - start - end) as i64)
}

/// `Some((ajoutées, retirées))` pour un outil qui modifie un fichier.
pub fn for_tool(name: &str, input: &Value) -> Option<(i64, i64)> {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
    match name {
        "Edit" => Some(lines(s("old_string"), s("new_string"))),
        "MultiEdit" => {
            let edits = input.get("edits")?.as_array()?;
            Some(edits.iter().fold((0, 0), |(a, r), e| {
                let (x, y) = lines(e["old_string"].as_str().unwrap_or(""), e["new_string"].as_str().unwrap_or(""));
                (a + x, r + y)
            }))
        }
        // Write écrit le fichier entier : son contenu antérieur n'est pas dans l'entrée.
        "Write" => Some((s("content").lines().count() as i64, 0)),
        "NotebookEdit" => Some((s("new_source").lines().count() as i64, 0)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts() {
        assert_eq!(lines("a\nb\nc", "a\nB\nc"), (1, 1));
        assert_eq!(lines("a\nb", "a\nb\nc\nd"), (2, 0));
        assert_eq!(lines("x\ny\nz", "x"), (0, 2));
        assert_eq!(lines("", "n1\nn2"), (2, 0));
        assert_eq!(for_tool("Write", &json!({"content": "1\n2\n3\n"})), Some((3, 0)));
        assert_eq!(for_tool("MultiEdit", &json!({"edits": [{"old_string": "a", "new_string": "b"}, {"old_string": "", "new_string": "c"}]})), Some((2, 1)));
        assert_eq!(for_tool("Bash", &json!({})), None);
    }
}
