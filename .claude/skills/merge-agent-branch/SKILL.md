---
name: merge-agent-branch
description: Злиття гілки агента (worktree-гілки на кшталт t3/…) у main Slicenv. Запускай, лише коли людина явно попросила злити гілку; також коли `git merge` дав конфлікт у crates/app/locales/en.json.
---

# Злиття гілки агента в main

Зливати в main і пушити — лише з явного дозволу людини. Дозвіл на одне злиття не поширюється на наступні.

1. **Підготовка.** Робоче дерево чисте (`git status --short` порожній). Гілка агента не має незакомічених змін у своєму worktree.
2. **Перевірка гілки до злиття** (у worktree гілки): `bash .claude/skills/verify/verify.sh`. Упало — не зливати, повернути агентові.
3. **Злиття** (у checkout, де стоїть main):
   ```bash
   git switch main
   git merge --no-ff --no-commit <гілка>
   ```
4. **Конфлікт у `crates/app/locales/en.json`** — типовий: обидві гілки дописали ключі в кінець. Не правити маркери вручну:
   ```bash
   python3 -I .claude/skills/merge-agent-branch/merge_en_json.py
   git add crates/app/locales/en.json
   ```
   Скрипт бере ключі ours (`git show :2:…`) у їхньому порядку, потім нові ключі theirs (`:3:`) у кінець; не сортує. Вихід 3 — той самий ключ має різні переклади: вибрати правильний вручну.
   Інші конфлікти — розв’язувати по суті; незрозуміло — `git merge --abort` і спитати людину.
5. **Перевірка результату** до коміту: `bash .claude/skills/verify/verify.sh`. Переклади обов’язково: об’єднання ключів могло загубити рядок.
6. **Коміт** через `-m`, щоб git не дописав блок `# Conflicts:` після `Co-Authored-By`:
   ```bash
   git commit -m "Злиття <що зроблено>" -m "Co-Authored-By: <модель> <noreply@…>"
   ```
7. Не пушити без окремого дозволу. Після злиття — видалити `target/` у worktree гілки (`rm -rf <worktree>/target`).
