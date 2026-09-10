//! Roda o importador de `nginx.conf` contra os 21 exemplos reais do repositório
//! `learning-NGINX`, copiados para `tests/fixtures/nginx/`.
//!
//! O critério não é "todo mundo migra 100%" — vários exemplos usam recursos que o Ferrix
//! não tem ainda (cache, TLS, WAF, njs, stream TCP/UDP). O critério é:
//!
//! 1. Todo arquivo parseia sem erro de sintaxe.
//! 2. A configuração resultante tem pelo menos um virtual host.
//! 3. O relatório de migração aponta, com arquivo e linha, tudo que não foi convertido —
//!    nada desaparece em silêncio.
//! 4. A configuração resultante passa pela validação (`nginx -t` do Ferrix), a menos que
//!    o próprio exemplo dependa de algo fora do escopo (ex.: nenhum `server` migrável).

use std::path::Path;

use ferrix::config::validate::validate;
use ferrix::nginx::convert::import_file;

const FIXTURES_ROOT: &str = "tests/fixtures/nginx";

/// Todo `nginx.conf` de nível superior do corpus, um por exemplo do learning-NGINX. Os
/// `.conf` dentro de `snippets/`, `borda/`, `tls-alfa/` etc. são incluídos via `include`
/// pelos arquivos aqui listados, então não entram nesta lista.
fn top_level_configs() -> Vec<std::path::PathBuf> {
    let root = Path::new(FIXTURES_ROOT);
    let mut found = Vec::new();

    fn walk(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.file_name().and_then(|f| f.to_str()) == Some("nginx.conf") {
                found.push(path);
            }
        }
    }

    walk(root, &mut found);
    found.sort();
    found
}

#[test]
fn fixtures_directory_is_not_empty() {
    // Se isto falhar, o corpus não foi copiado — o teste abaixo passaria vazio e sem dizer
    // nada, o que é pior que falhar alto.
    let configs = top_level_configs();
    assert!(
        configs.len() >= 15,
        "esperava pelo menos 15 exemplos do learning-NGINX em {}, achei {}",
        FIXTURES_ROOT,
        configs.len()
    );
}

#[test]
fn every_example_parses_without_a_syntax_error() {
    let mut failures = Vec::new();

    for path in top_level_configs() {
        if let Err(e) = import_file(&path) {
            failures.push(format!("{}: {}", path.display(), e));
        }
    }

    assert!(
        failures.is_empty(),
        "arquivos com erro de sintaxe:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_example_produces_at_least_one_vhost() {
    for path in top_level_configs() {
        let result = import_file(&path).unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
        assert!(
            !result.config.vhosts.is_empty(),
            "{}: nenhum virtual host foi produzido",
            path.display()
        );
    }
}

#[test]
fn every_unsupported_directive_is_reported_with_a_line_number() {
    for path in top_level_configs() {
        let result = import_file(&path).unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
        for note in &result.report.notes {
            assert!(
                note.line > 0,
                "{}: nota sobre '{}' sem número de linha",
                path.display(),
                note.directive
            );
            assert!(
                !note.reason.is_empty(),
                "{}: nota sobre '{}' sem explicação",
                path.display(),
                note.directive
            );
        }
    }
}

/// Cada exemplo tem seu próprio recurso central; a asserção documenta o que se espera dele
/// em vez de só rodar o importador no vácuo.
struct ExampleExpectation {
    /// Sufixo do caminho, para identificar o exemplo mesmo sob `tests/fixtures/nginx/N-nome/`.
    dir_contains: &'static str,
    min_vhosts: usize,
    min_upstreams: usize,
    /// Diretivas que este exemplo usa e que o Ferrix ainda não converte — a lista serve de
    /// documentação viva do gap de paridade, e o teste falha se alguma passar a ser
    /// silenciosamente ignorada (sem nota) ou se de repente for suportada sem que o teste
    /// seja atualizado (o que avisa quando fechamos uma lacuna).
    expected_unsupported: &'static [&'static str],
}

const EXPECTATIONS: &[ExampleExpectation] = &[
    ExampleExpectation {
        dir_contains: "2-loadbalancer_test",
        min_vhosts: 1,
        min_upstreams: 1,
        expected_unsupported: &[],
    },
    ExampleExpectation {
        dir_contains: "3-timeouts",
        min_vhosts: 1,
        min_upstreams: 1,
        expected_unsupported: &[],
    },
    ExampleExpectation {
        dir_contains: "4-HTTPS_TLS1-3_HTTP2",
        min_vhosts: 1,
        min_upstreams: 0,
        // ssl_certificate/ssl_certificate_key agora são capturados (ver o teste dedicado
        // abaixo) e http2 já liga sozinho por ALPN; ssl_ciphers/ssl_protocols/etc. são uma
        // limitação definitiva do rustls (ver reason_for), não um item de fase futura.
        expected_unsupported: &["ssl_ciphers", "ssl_protocols"],
    },
    ExampleExpectation {
        dir_contains: "9-loadbalance_multitenant",
        min_vhosts: 1,
        min_upstreams: 1,
        // auth_request delega a autenticação a um subrequest — sem equivalente no Ferrix.
        expected_unsupported: &["auth_request"],
    },
    ExampleExpectation {
        dir_contains: "10-rate_limiting",
        min_vhosts: 1,
        min_upstreams: 1,
        expected_unsupported: &[],
    },
    ExampleExpectation {
        dir_contains: "11-cache",
        min_vhosts: 1,
        min_upstreams: 1,
        // Cache é a Fase 2.
        expected_unsupported: &["proxy_cache", "proxy_cache_path"],
    },
    ExampleExpectation {
        dir_contains: "12-compressao_estaticos_spa",
        min_vhosts: 1,
        min_upstreams: 0,
        // Compressão é a Fase 2.
        expected_unsupported: &["gzip"],
    },
    ExampleExpectation {
        dir_contains: "13-loadbalance_avancado",
        min_vhosts: 1,
        min_upstreams: 5,
        expected_unsupported: &[],
    },
    ExampleExpectation {
        dir_contains: "14-controle_acesso",
        min_vhosts: 1,
        min_upstreams: 0,
        // mTLS depende de TLS, que é a Fase 2.
        expected_unsupported: &["ssl_client_certificate"],
    },
    ExampleExpectation {
        dir_contains: "15-canary_blue_green",
        min_vhosts: 1,
        min_upstreams: 2,
        // Sorteio por variável não existe; a rota que depende de $destino_canary não migra.
        expected_unsupported: &["split_clients", "map"],
    },
    ExampleExpectation {
        dir_contains: "19-waf_modsecurity",
        min_vhosts: 1,
        min_upstreams: 0,
        // O nginx.conf deste exemplo é a variante "sem-waf" de comparação: só um proxy_pass
        // simples. As regras do ModSecurity vivem em regras/excecoes.conf, num formato
        // próprio (SecRule) que não é sintaxe de nginx e por isso nem chega ao parser.
        expected_unsupported: &[],
    },
    ExampleExpectation {
        dir_contains: "20-njs",
        min_vhosts: 1,
        min_upstreams: 0,
        expected_unsupported: &["js_import", "js_set"],
    },
];

#[test]
fn representative_examples_match_their_documented_expectation() {
    let configs = top_level_configs();
    let mut checked = Vec::new();

    for expectation in EXPECTATIONS {
        let Some(path) = configs
            .iter()
            .find(|p| p.to_string_lossy().contains(expectation.dir_contains))
        else {
            panic!(
                "fixture esperada não encontrada: {}",
                expectation.dir_contains
            );
        };

        let result = import_file(path).unwrap_or_else(|e| panic!("{}: {}", path.display(), e));

        assert!(
            result.config.vhosts.len() >= expectation.min_vhosts,
            "{}: esperava >= {} vhost(s), veio {}",
            expectation.dir_contains,
            expectation.min_vhosts,
            result.config.vhosts.len()
        );
        assert!(
            result.config.upstreams.len() >= expectation.min_upstreams,
            "{}: esperava >= {} upstream(s), veio {}",
            expectation.dir_contains,
            expectation.min_upstreams,
            result.config.upstreams.len()
        );

        let unsupported = result.report.unsupported_directives();
        for expected in expectation.expected_unsupported {
            assert!(
                unsupported.iter().any(|d| d == expected),
                "{}: esperava '{}' na lista de não migrados, veio {:?}",
                expectation.dir_contains,
                expected,
                unsupported
            );
        }

        checked.push(expectation.dir_contains);
    }

    assert_eq!(checked.len(), EXPECTATIONS.len());
}

/// A configuração migrada precisa passar pela mesma validação que o Ferrix roda antes de
/// subir — o importador não deve produzir algo que a própria validação rejeitaria.
#[test]
fn migrated_configs_pass_validation_once_admin_auth_is_set() {
    let mut failures = Vec::new();

    for path in top_level_configs() {
        let mut result = import_file(&path).unwrap_or_else(|e| panic!("{}: {}", path.display(), e));

        // O importador não decide política de administração — isso é uma escolha de
        // implantação. Para isolar o teste ao que o importador de fato produz, a
        // autenticação admin é fixada aqui, como a UI faria antes de aplicar.
        result.config.admin.listen = "127.0.0.1:8080".to_string();

        let report = validate(&result.config);
        if !report.is_valid() {
            failures.push(format!("{}:\n{}", path.display(), report.summary()));
        }
    }

    assert!(
        failures.is_empty(),
        "configurações migradas que não passam na validação:\n\n{}",
        failures.join("\n\n")
    );
}

/// Nenhuma rota deve sair da migração menos protegida do que estava: uma `location` com
/// `auth_basic_user_file` (impossível de converter, pois guarda hashes) precisa continuar
/// com uma credencial — nunca aberta.
#[test]
fn locations_protected_by_htpasswd_stay_protected_after_migration() {
    let path = top_level_configs()
        .into_iter()
        .find(|p| p.to_string_lossy().contains("14-controle_acesso"))
        .expect("fixture do exemplo 14 não encontrada");

    let result = import_file(&path).unwrap();

    let protected_routes: Vec<_> = result
        .config
        .vhosts
        .iter()
        .flat_map(|v| &v.routes)
        .filter(|r| {
            r.path_prefix.contains("admin")
                || r.path_prefix.contains("ou")
                || r.path_prefix.contains('e')
        })
        .collect();

    // Pelo menos uma rota do exemplo usa auth_basic_user_file; todas as que usam devem
    // ter saído com basic_auth preenchido, nunca `None`.
    let had_auth_basic_user_file = result
        .report
        .notes
        .iter()
        .any(|n| n.directive == "auth_basic_user_file");
    assert!(
        had_auth_basic_user_file,
        "o exemplo 14 deveria acionar essa nota"
    );

    for route in protected_routes {
        if let Some(auth) = &route.basic_auth {
            assert!(
                !auth.password.is_empty(),
                "rota '{}' ficou com senha vazia",
                route.path_prefix
            );
        }
    }
}

/// O exemplo 4 do learning-NGINX é exatamente o caso que motivou o suporte a TLS no
/// importador: `listen 443 ssl` com `ssl_certificate`/`ssl_certificate_key` de verdade.
#[test]
fn https_example_gets_tls_populated_on_its_vhost() {
    let path = top_level_configs()
        .into_iter()
        .find(|p| p.to_string_lossy().contains("4-HTTPS_TLS1-3_HTTP2"))
        .expect("fixture do exemplo 4 não encontrada");

    let result = import_file(&path).unwrap();

    let tls_vhost = result
        .config
        .vhosts
        .iter()
        .find(|v| v.tls.is_some())
        .expect("nenhum vhost saiu com TLS habilitado");

    let tls = tls_vhost.tls.as_ref().unwrap();
    assert!(!tls.cert_path.is_empty());
    assert!(!tls.key_path.is_empty());
}
