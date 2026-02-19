#!/bin/bash
# Simulated infinity-agent demo — outputs colored terminal UI
# Run under asciinema to capture as a recording

set -e

# Colors / ANSI
RST="\033[0m"
BOLD="\033[1m"
DIM="\033[2m"
ITALIC="\033[3m"
UL="\033[4m"

BLACK="\033[30m"
RED="\033[31m"
GREEN="\033[32m"
YELLOW="\033[33m"
BLUE="\033[34m"
MAGENTA="\033[35m"
CYAN="\033[36m"
WHITE="\033[37m"
GREY="\033[90m"

BG_SURFACE="\033[48;2;26;29;39m"

# Helpers
type_text() {
  local text="$1" delay="${2:-0.04}"
  for (( i=0; i<${#text}; i++ )); do
    printf '%s' "${text:$i:1}"
    sleep "$delay"
  done
}

spinner_frames=("⠋" "⠙" "⠹" "⠸" "⠼" "⠴" "⠦" "⠧" "⠇" "⠏")
spin() {
  local msg="$1" color="$2" duration="$3"
  local start=$SECONDS
  local i=0
  while (( SECONDS - start < duration )); do
    local elapsed=$(( SECONDS - start ))
    printf "\r${color}${BOLD}  ${spinner_frames[$((i % 10))]} ${msg} ${GREY}(${elapsed}.$(( (i*2) % 10 ))s)${RST}     "
    sleep 0.1
    ((i++))
  done
  printf "\r\033[2K"
}

clear
echo ""
printf "${MAGENTA}${BOLD}"
cat << 'LOGO'
  ██╗███╗   ██╗███████╗██╗███╗   ██╗██╗████████╗██╗   ██╗
  ██║████╗  ██║██╔════╝██║████╗  ██║██║╚══██╔══╝╚██╗ ██╔╝
  ██║██╔██╗ ██║█████╗  ██║██╔██╗ ██║██║   ██║    ╚████╔╝
  ██║██║╚██╗██║██╔══╝  ██║██║╚██╗██║██║   ██║     ╚██╔╝
  ██║██║ ╚████║██║     ██║██║ ╚████║██║   ██║      ██║
  ╚═╝╚═╝  ╚═══╝╚═╝     ╚═╝╚═╝  ╚═══╝╚═╝   ╚═╝      ╚═╝
LOGO
printf "${RST}"
printf "  ${DIM}AI Coding Agent • v0.2.0 • claude-sonnet-4-6${RST}\n"
echo ""
printf "  ${GREEN}✓${RST} Session ${CYAN}a1b2c3d4${RST} loaded (0 messages)\n"
printf "  ${GREEN}✓${RST} Memory: 3 playbook entries, 2 episodes\n"
printf "  ${GREEN}✓${RST} Tools: read_file, write_file, bash, list_files, search, kv_get, kv_set\n"
echo ""
printf "  ${DIM}Type ${WHITE}/help${DIM} for commands • ${WHITE}Ctrl+C${DIM} to cancel • ${WHITE}Ctrl+D${DIM} to exit${RST}\n"
echo ""
sleep 1.5

# ─── User prompt ───
printf "${BOLD}${BLUE}  ❯ ${RST}"
type_text "Build me a TaskFlow web app — a modern task manager with dark theme, priority levels, filters, and stats cards. Serve it on port 3456." 0.03
echo ""
echo ""
sleep 0.8

# ─── Step 1: Thinking ───
spin "Thinking" "\033[35m" 3

# ─── Text streaming (prose) ───
printf "  ${RST}"
prose="I'll build a TaskFlow web app for you — a sleek task manager with a dark theme. Let me start by planning the structure, then write the code.\n\n  Here's my plan:\n  1. Create the project directory\n  2. Write the HTML with embedded CSS and JavaScript\n  3. Start a local server on port 3456\n\n  Let me get started."
for (( i=0; i<${#prose}; i++ )); do
  char="${prose:$i:1}"
  if [ "$char" = $'\n' ]; then
    echo ""
  else
    printf '%s' "$char"
  fi
  sleep 0.015
done
echo ""
echo ""
sleep 0.5

# ─── Preparing actions spinner ───
spin "Preparing actions" "\033[36m" 2

# ─── Tool 1: bash (mkdir) ───
printf "  ${BOLD}${YELLOW}▸ [1/4]${RST} ${BOLD}bash${RST} ${GREY}mkdir -p taskflow${RST}\n"
spin "Creating directory" "\033[33m" 1
printf "  ${DIM}${GREY}  Created taskflow/${RST}\n"
printf "  ${GREEN}  ✓ ${DIM}0.3s${RST}\n"
echo ""
sleep 0.3

# ─── Tool 2: write_file ───
printf "  ${BOLD}${GREEN}▸ [2/4]${RST} ${BOLD}write_file${RST} ${GREY}taskflow/index.html${RST}\n"
spin "Writing taskflow/index.html" "\033[32m" 2

# Show code block with borders
printf "  ${CYAN}┌─ html${RST}\n"
sleep 0.1

code_lines=(
  '<!DOCTYPE html>'
  '<html lang="en">'
  '<head>'
  '  <meta charset="UTF-8">'
  '  <title>TaskFlow — Smart Task Manager</title>'
  '  <style>'
  '    :root {'
  '      --bg: #0f1117;'
  '      --surface: #1a1d27;'
  '      --accent: #818cf8;'
  '      --green: #34d399;'
  '    }'
  '    /* ... 120 lines of CSS ... */'
  '  </style>'
  '</head>'
  '<body>'
  '  <div class="app">'
  '    <header>'
  '      <h1>TaskFlow</h1>'
  '    </header>'
  '    <div class="stats" id="stats"></div>'
  '    <form class="add-form" id="addForm">'
  '      <input placeholder="What needs to be done?">'
  '      <select id="priorityInput">...</select>'
  '      <button type="submit">Add</button>'
  '    </form>'
  '    <div class="task-list" id="taskList"></div>'
  '  </div>'
  '  <script>'
  '    let tasks = JSON.parse(localStorage.getItem(...));'
  '    function render() { /* ... */ }'
  '    function toggle(id) { /* ... */ }'
  '    render();'
  '  </script>'
  '</body>'
  '</html>'
)

for line in "${code_lines[@]}"; do
  printf "  ${CYAN}│${RST} ${DIM}%s${RST}\n" "$line"
  sleep 0.06
done
printf "  ${CYAN}└─${RST}\n"
sleep 0.2

printf "  ${DIM}${GREY}  Wrote 180 lines to taskflow/index.html${RST}\n"
printf "  ${GREEN}  ✓ ${DIM}1.2s${RST}\n"
echo ""
sleep 0.3

# ─── Step 2: Reviewing results ───
printf "  ${MAGENTA}${BOLD}  ↻ continuing${RST} ${GREY}(step 2)${RST}\n"
spin "Reviewing results" "\033[34m" 2

printf "  ${RST}"
prose2="The HTML file is ready with a complete dark-themed UI. Now let me verify the code and start the server."
for (( i=0; i<${#prose2}; i++ )); do
  printf '%s' "${prose2:$i:1}"
  sleep 0.02
done
echo ""
echo ""
sleep 0.3

spin "Preparing actions" "\033[36m" 1

# ─── Tool 3: bash (validate) ───
printf "  ${BOLD}${YELLOW}▸ [3/4]${RST} ${BOLD}bash${RST} ${GREY}wc -l taskflow/index.html${RST}\n"
spin "Checking file" "\033[33m" 1
printf "  ${DIM}${GREY}  180 taskflow/index.html${RST}\n"
printf "  ${GREEN}  ✓ ${DIM}0.1s${RST}\n"
echo ""
sleep 0.3

# ─── Tool 4: bash (start server) ───
printf "  ${BOLD}${YELLOW}▸ [4/4]${RST} ${BOLD}bash${RST} ${GREY}python3 -m http.server 3456 -d taskflow &${RST}\n"
spin "Starting server" "\033[33m" 2
printf "  ${DIM}${GREY}  Serving HTTP on 0.0.0.0 port 3456${RST}\n"
printf "  ${GREEN}  ✓ ${DIM}0.8s${RST}\n"
echo ""
sleep 0.3

# ─── Step 3: Connecting the dots ───
printf "  ${MAGENTA}${BOLD}  ↻ continuing${RST} ${GREY}(step 3)${RST}\n"
spin "Connecting the dots" "\033[36m" 2

# ─── Final response ───
printf "  ${RST}"
final="Your TaskFlow app is live! Here's what I built:\n\n"
for (( i=0; i<${#final}; i++ )); do
  char="${final:$i:1}"
  if [ "$char" = $'\n' ]; then echo ""; else printf '%s' "$char"; fi
  sleep 0.02
done

printf "  ${GREEN}${BOLD}  ✦ Features:${RST}\n"
sleep 0.1
printf "    ${DIM}• Dark theme with CSS custom properties${RST}\n"
sleep 0.1
printf "    ${DIM}• Add/complete/delete tasks with animations${RST}\n"
sleep 0.1
printf "    ${DIM}• Priority levels (high/medium/low) with color badges${RST}\n"
sleep 0.1
printf "    ${DIM}• Filter by status or priority${RST}\n"
sleep 0.1
printf "    ${DIM}• Stats dashboard (total/completed/urgent/remaining)${RST}\n"
sleep 0.1
printf "    ${DIM}• localStorage persistence${RST}\n"
echo ""
sleep 0.2

printf "  ${CYAN}${BOLD}  → Open ${UL}http://localhost:3456${RST}${CYAN}${BOLD} in your browser${RST}\n"
echo ""
sleep 0.3

# ─── Token usage ───
printf "  ${GREY}─────────────────────────────────────────────${RST}\n"
printf "  ${GREY}tokens: ${WHITE}12.4k in${GREY} · ${WHITE}3.8k out${GREY} · cost: ${GREEN}\$0.024${GREY} · session: ${WHITE}16.2k${GREY} (${GREEN}\$0.024${GREY})${RST}\n"
echo ""
echo ""

# ─── Second prompt ───
printf "${BOLD}${BLUE}  ❯ ${RST}"
type_text "Can you add a progress bar showing completion percentage?" 0.03
echo ""
echo ""
sleep 0.8

spin "Thinking" "\033[35m" 2

printf "  ${RST}"
resp="Sure! I'll add a progress bar below the stats cards."
for (( i=0; i<${#resp}; i++ )); do
  printf '%s' "${resp:$i:1}"
  sleep 0.02
done
echo ""
echo ""
sleep 0.3

spin "Preparing actions" "\033[36m" 1

printf "  ${BOLD}${GREEN}▸ [1/1]${RST} ${BOLD}write_file${RST} ${GREY}taskflow/index.html (patch)${RST}\n"
spin "Editing taskflow/index.html" "\033[32m" 2

# Show diff-style output
printf "  ${CYAN}┌─ diff${RST}\n"
sleep 0.1
printf "  ${CYAN}│${RST} ${CYAN}@@ -45,6 +45,18 @@${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}  ${DIM}  </div>${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+ <div class=\"progress-bar\">${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+   <div class=\"progress-fill\" id=\"progressFill\"></div>${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+   <span class=\"progress-text\" id=\"progressText\">40%%</span>${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+ </div>${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST} ${DIM}  <form class=\"add-form\">${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}\n"
printf "  ${CYAN}│${RST} ${CYAN}@@ -120,6 +132,15 @@${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+ .progress-bar {${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+   height: 6px; background: var(--border);${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+   border-radius: 3px; margin-bottom: 1.5rem;${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+   position: relative;${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+ }${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+ .progress-fill {${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+   height: 100%%; background: var(--accent);${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+   border-radius: 3px; transition: width 0.4s;${RST}\n"
sleep 0.05
printf "  ${CYAN}│${RST}${GREEN}${BOLD}+ }${RST}\n"
sleep 0.05
printf "  ${CYAN}└─${RST}\n"

printf "  ${DIM}${GREY}  Patched 18 lines in taskflow/index.html${RST}\n"
printf "  ${GREEN}  ✓ ${DIM}0.9s${RST}\n"
echo ""
sleep 0.3

printf "  ${MAGENTA}${BOLD}  ↻ continuing${RST} ${GREY}(step 2)${RST}\n"
spin "Synthesizing" "\033[34m" 1

printf "  ${RST}Done! The progress bar now shows completion percentage with a smooth animated fill.\n"
printf "  ${RST}Refresh ${CYAN}${UL}http://localhost:3456${RST} to see the update.\n"
echo ""
sleep 0.3

printf "  ${GREY}─────────────────────────────────────────────${RST}\n"
printf "  ${GREY}tokens: ${WHITE}8.2k in${GREY} · ${WHITE}2.1k out${GREY} · cost: ${GREEN}\$0.015${GREY} · session: ${WHITE}26.5k${GREY} (${GREEN}\$0.039${GREY})${RST}\n"
echo ""
echo ""

# ─── Ctrl+C demo ───
printf "${BOLD}${BLUE}  ❯ ${RST}"
type_text "Refactor the entire CSS to use Tailwind instead" 0.03
echo ""
echo ""
sleep 0.8

spin "Thinking" "\033[35m" 2

printf "  ${RST}I'll refactor the CSS to use Tailwind. First let me "
sleep 0.5

# Simulate Ctrl+C
printf "\n"
printf "  ${YELLOW}  ^C — cancelled${RST}\n"
echo ""
sleep 1.0

printf "${BOLD}${BLUE}  ❯ ${RST}"
type_text "Actually, the custom CSS is fine. Thanks!" 0.03
echo ""
echo ""
sleep 0.8

spin "Thinking" "\033[35m" 1

printf "  ${RST}You're welcome! The custom CSS keeps the app dependency-free and loads instantly.\n"
printf "  ${RST}The TaskFlow app is running at ${CYAN}${UL}http://localhost:3456${RST} — enjoy! \n"
echo ""

printf "  ${GREY}─────────────────────────────────────────────${RST}\n"
printf "  ${GREY}tokens: ${WHITE}4.1k in${GREY} · ${WHITE}0.6k out${GREY} · cost: ${GREEN}\$0.005${GREY} · session: ${WHITE}31.2k${GREY} (${GREEN}\$0.044${GREY})${RST}\n"
echo ""
echo ""

printf "${BOLD}${BLUE}  ❯ ${RST}"
sleep 2
printf "\n"
printf "  ${DIM}Session saved. Goodbye!${RST}\n"
echo ""
