//! Relatório da migração de um `nginx.conf`.
//!
//! O importador não inventa equivalências. Toda diretiva que ele não sabe traduzir vira
//! uma linha aqui, com arquivo, número de linha, texto original e o motivo — porque uma
//! migração que some silenciosamente com regras de cache, TLS ou WAF é pior que uma
//! migração que não acontece.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteLevel {
    /// Convertido integralmente. Registrado só quando vale explicar como.
    Converted,
    /// Convertido, mas com diferença de comportamento que precisa ser conferida.
    Partial,
    /// Sem equivalente no Ferrix. Continua valendo no nginx e some na migração — um gap de
    /// verdade, candidato a fase futura.
    Unsupported,
    /// Não faz nada de propósito, não por falta de tempo: a diretiva ajusta algo que o
    /// Ferrix decidiu não expor (ex.: `ssl_dhparam` — o rustls não implementa DHE
    /// clássico). Diferente de `Unsupported`, não é um item pendente.
    Ignored,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationNote {
    pub level: NoteLevel,
    pub file: String,
    pub line: usize,
    pub directive: String,
    /// O texto original, para que dê para procurar no arquivo de origem.
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MigrationReport {
    pub files: Vec<String>,
    pub notes: Vec<MigrationNote>,
    pub vhosts_created: usize,
    pub routes_created: usize,
    pub upstreams_created: usize,
}

impl MigrationReport {
    pub fn note(
        &mut self,
        level: NoteLevel,
        file: &str,
        line: usize,
        directive: &str,
        source: &str,
        reason: impl Into<String>,
    ) {
        self.notes.push(MigrationNote {
            level,
            file: file.to_string(),
            line,
            directive: directive.to_string(),
            source: source.to_string(),
            reason: reason.into(),
        });
    }

    pub fn unsupported(
        &mut self,
        file: &str,
        line: usize,
        directive: &str,
        source: &str,
        reason: impl Into<String>,
    ) {
        self.note(
            NoteLevel::Unsupported,
            file,
            line,
            directive,
            source,
            reason,
        );
    }

    pub fn partial(
        &mut self,
        file: &str,
        line: usize,
        directive: &str,
        source: &str,
        reason: impl Into<String>,
    ) {
        self.note(NoteLevel::Partial, file, line, directive, source, reason);
    }

    pub fn ignored(
        &mut self,
        file: &str,
        line: usize,
        directive: &str,
        source: &str,
        reason: impl Into<String>,
    ) {
        self.note(NoteLevel::Ignored, file, line, directive, source, reason);
    }

    pub fn count(&self, level: NoteLevel) -> usize {
        self.notes.iter().filter(|n| n.level == level).count()
    }

    /// Nomes de diretivas não migradas, sem repetição, para o resumo.
    pub fn unsupported_directives(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .notes
            .iter()
            .filter(|n| n.level == NoteLevel::Unsupported)
            .map(|n| n.directive.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Relatório de migração — nginx para Ferrix\n\n");

        out.push_str("## Arquivos lidos\n\n");
        if self.files.is_empty() {
            out.push_str("_nenhum_\n");
        } else {
            for f in &self.files {
                out.push_str(&format!("- `{}`\n", f));
            }
        }

        out.push_str(&format!(
            "\n## Resumo\n\n\
             | O quê | Quantidade |\n\
             |---|---|\n\
             | Virtual hosts criados | {} |\n\
             | Rotas criadas | {} |\n\
             | Upstreams criados | {} |\n\
             | Diretivas migradas com ressalva | {} |\n\
             | Diretivas **não** migradas | {} |\n\
             | Diretivas ignoradas por design (permanente, não é pendência) | {} |\n",
            self.vhosts_created,
            self.routes_created,
            self.upstreams_created,
            self.count(NoteLevel::Partial),
            self.count(NoteLevel::Unsupported),
            self.count(NoteLevel::Ignored),
        ));

        let unsupported = self.unsupported_directives();
        if !unsupported.is_empty() {
            out.push_str(&format!(
                "\nDiretivas sem equivalente: {}\n",
                unsupported
                    .iter()
                    .map(|d| format!("`{}`", d))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        for (level, title, intro) in [
            (
                NoteLevel::Unsupported,
                "Não migrado",
                "Estas diretivas continuam valendo no nginx e **não** têm efeito no Ferrix. \
                 Confira cada uma antes de trocar o servidor de lugar.",
            ),
            (
                NoteLevel::Partial,
                "Migrado com ressalva",
                "Convertido, mas com alguma diferença de comportamento que vale conferir.",
            ),
            (
                NoteLevel::Ignored,
                "Ignorado por design",
                "Estas diretivas não têm efeito no Ferrix de propósito, não por falta de \
                 tempo — geralmente porque o Ferrix já toma essa decisão sozinha, de forma \
                 mais segura. Não é uma pendência.",
            ),
            (
                NoteLevel::Converted,
                "Notas de conversão",
                "Decisões tomadas durante a conversão que é bom saber.",
            ),
        ] {
            let notes: Vec<&MigrationNote> =
                self.notes.iter().filter(|n| n.level == level).collect();
            if notes.is_empty() {
                continue;
            }

            out.push_str(&format!("\n## {}\n\n{}\n\n", title, intro));
            out.push_str(
                "| Arquivo | Linha | Diretiva | Original | Motivo |\n|---|---|---|---|---|\n",
            );
            for n in notes {
                out.push_str(&format!(
                    "| `{}` | {} | `{}` | `{}` | {} |\n",
                    n.file,
                    n.line,
                    n.directive,
                    escape_pipes(&n.source),
                    n.reason
                ));
            }
        }

        out
    }

    /// Resumo curto para o terminal.
    pub fn to_text_summary(&self) -> String {
        let mut lines = vec![format!(
            "{} virtual host(s), {} rota(s), {} upstream(s) criados",
            self.vhosts_created, self.routes_created, self.upstreams_created
        )];

        let unsupported = self.count(NoteLevel::Unsupported);
        let partial = self.count(NoteLevel::Partial);
        if unsupported == 0 && partial == 0 {
            lines.push("todas as diretivas encontradas foram migradas".to_string());
        } else {
            lines.push(format!(
                "{} diretiva(s) não migrada(s), {} com ressalva",
                unsupported, partial
            ));
            for name in self.unsupported_directives() {
                let count = self
                    .notes
                    .iter()
                    .filter(|n| n.level == NoteLevel::Unsupported && n.directive == name)
                    .count();
                lines.push(format!("  - {} ({}x)", name, count));
            }
        }
        lines.join("\n")
    }
}

/// A barra vertical quebraria a tabela markdown.
fn escape_pipes(s: &str) -> String {
    s.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MigrationReport {
        let mut r = MigrationReport::default();
        r.files.push("nginx.conf".into());
        r.vhosts_created = 1;
        r.routes_created = 3;
        r.upstreams_created = 2;
        r.unsupported(
            "nginx.conf",
            12,
            "proxy_cache",
            "proxy_cache zona;",
            "cache ainda não existe no Ferrix",
        );
        r.unsupported(
            "nginx.conf",
            20,
            "gzip",
            "gzip on;",
            "compressão ainda não existe",
        );
        r.unsupported(
            "nginx.conf",
            30,
            "proxy_cache",
            "proxy_cache outra;",
            "cache ainda não existe no Ferrix",
        );
        r.partial(
            "nginx.conf",
            40,
            "listen",
            "listen 443 ssl;",
            "TLS ainda não existe; a porta foi migrada",
        );
        r
    }

    #[test]
    fn counts_by_level() {
        let r = sample();
        assert_eq!(r.count(NoteLevel::Unsupported), 3);
        assert_eq!(r.count(NoteLevel::Partial), 1);
        assert_eq!(r.count(NoteLevel::Converted), 0);
    }

    #[test]
    fn unsupported_directive_names_are_deduplicated() {
        assert_eq!(
            sample().unsupported_directives(),
            vec!["gzip".to_string(), "proxy_cache".to_string()]
        );
    }

    #[test]
    fn markdown_mentions_every_note_with_its_line() {
        let md = sample().to_markdown();
        assert!(md.contains("proxy_cache"));
        assert!(md.contains("gzip"));
        assert!(md.contains("| 12 |"));
        assert!(md.contains("Não migrado"));
        assert!(md.contains("Migrado com ressalva"));
    }

    #[test]
    fn markdown_escapes_pipes_so_the_table_survives() {
        let mut r = MigrationReport::default();
        r.unsupported(
            "f.conf",
            1,
            "map",
            "map $a $b { default a|b; }",
            "sem equivalente",
        );
        let md = r.to_markdown();
        assert!(md.contains(r"a\|b"));
    }

    #[test]
    fn empty_report_says_everything_migrated() {
        let r = MigrationReport::default();
        assert!(r.to_text_summary().contains("todas as diretivas"));
    }

    #[test]
    fn text_summary_lists_counts_per_directive() {
        let summary = sample().to_text_summary();
        assert!(summary.contains("proxy_cache (2x)"));
        assert!(summary.contains("gzip (1x)"));
    }
}
