# Regras do Projeto (AGENTS.md)

Este repositório segue estritamente as diretrizes definidas em [GEMINI.md](file:///d:/rust/duckdns-updater/GEMINI.md):

1. **Versionamento Obrigatório:** Toda alteração de código ou fix exige incremento de versão no `Cargo.toml`.
2. **Build e Verificação de Ícone/PE:** Utilizar sempre `.\build_and_verify.ps1` para compilar em modo release com recursos e ícones PE validados.
3. **Cópia de Binários na Raiz:** Manter o executável atualizado diretamente na raiz do projeto (`duckdns-updater.exe`) e em `dist/duckdns-updater.exe`.
4. **Auto-Updater Funcional:** O executável possui auto-atualizador integrado apontando para `leandrop2103/duckdns-updater` (com fallback em `lpl2103/duckdns-updater`).
5. **Publicação no GitHub (Apenas Sob Demanda):**
   - **NÃO fazer push nem criar releases automaticamente.**
   - O push, tag e release no GitHub só devem ser executados quando o usuário pedir explicitamente.
