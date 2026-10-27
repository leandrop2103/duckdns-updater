# Regras de Desenvolvimento e Diretrizes do Projeto (`GEMINI.md`)

Este arquivo define os padrões obrigatórios e automáticos que o assistente de IA (Antigravity) deve seguir **sempre que fizer qualquer alteração ou build neste repositório**.

---

## 📌 1. Regra de Versionamento Obrigatório (SemVer)
- **Toda alteração de código, melhoria ou correção de bug DEVE obrigatoriamente incrementar o número da versão** no arquivo `Cargo.toml`.
- Exemplo: de `1.1.0` para `1.1.1`.
- Nunca gerar uma nova compilação de produção sem incrementar a versão, pois o auto-atualizador (`src/core/updater.rs`) depende de versões estritamente crescentes para notificar e atualizar os clientes.

---

## 🛠️ 2. Regra de Compilação e Recursos Windows (Ícone & PE Metadata)

> [!IMPORTANT]
> O executável deve conter o ícone nativo multi-resolução (`assets/icon.ico`) e metadados PE (`VERSIONINFO` completos: Empresa, Versão, Descrição, Copyright).
> Ao compilar, utilize sempre o script de verificação:
> ```powershell
> .\build_and_verify.ps1
> ```
> Ele compila em modo release, valida a tabela de recursos do PE e garante que o Windows Explorer reconheça o ícone e metadados.

---

## 📂 3. Cópia e Sincronização Obrigatória dos Binários
- **O executável buildado DEVE SEMPRE ser sincronizado na raiz do projeto (`duckdns-updater.exe`)** e na pasta `dist/duckdns-updater.exe`.
- O script `build_and_verify.ps1` já realiza essa sincronização automaticamente:
```powershell
Copy-Item -Force target\release\duckdns-updater.exe dist\duckdns-updater.exe
Copy-Item -Force target\release\duckdns-updater.exe duckdns-updater.exe
```

---

## 🔄 4. Auto-Updater e Substituição Quente (Hot-Swap)
- O auto-atualizador (`src/core/updater.rs`) consulta a API do GitHub (`leandrop2103/duckdns-updater/releases/latest`, com fallback para `lpl2103`).
- No Windows, a substituição em execução ocorre por rename atômico:
  1. Download do binário novo para `duckdns-updater.exe.new`.
  2. Executável em execução renomeado para `duckdns-updater.exe.old`.
  3. `duckdns-updater.exe.new` renomeado para `duckdns-updater.exe`.
  4. Reinicialização disparando o novo processo com a flag `--cleanup-old`.
  5. Função `clean_old_update_files()` em `main.rs` remove arquivos `.old` pendentes.

---

## 🛑 5. Regra de Publicação no GitHub (SOMENTE SOB DEMANDA)

> [!IMPORTANT]
> **NUNCA faça git push ou criação de releases automaticamente.**
> O envio para o repositório remoto e criação de release no GitHub devem ser feitos **apenas quando o usuário solicitar explicitamente**.
> Quando o usuário solicitar a publicação, execute:
```powershell
.\build_and_release.ps1 -Notes "Descrição das melhorias" -Push
```
Ou manualmente:
```powershell
git add Cargo.toml Cargo.lock duckdns-updater.exe dist/duckdns-updater.exe src/ build.rs README.md GEMINI.md AGENTS.md build_and_verify.ps1 build_and_release.ps1 .gitignore eframe-patched/ assets/
git commit -m "release: v<VERSÃO> - <Notas>"
git push origin master
git tag -a v<VERSÃO> -m "Release v<VERSÃO>"
git push origin v<VERSÃO>
gh release create v<VERSÃO> dist/duckdns-updater.exe --repo leandrop2103/duckdns-updater --title "DuckDNS Updater v<VERSÃO>" --notes "<Notas>"
```

---

## 🦀 6. Boas Práticas de Código e Estabilidade
1. **Zero Panics com Mutex:** Nunca utilizar `.lock().unwrap()`. Tratar eventuais poisonings com `.unwrap_or_else(|e| e.into_inner())`.
2. **Tratamento Resiliente de Erros:** Não utilizar `.expect()` ou `.unwrap()` em fluxos de rede, parsing de SemVer ou operações com o sistema de arquivos. Sempre propagar `Result<T, E>`.
3. **Persistência Segura DPAPI:** Tokens de domínio e dados sensíveis devem ser protegidos pela API nativa de criptografia do Windows quando salvos em disco.
4. **Clippy 100% Limpo:** Manter o projeto sem nenhum warning do Clippy (`cargo clippy`).
