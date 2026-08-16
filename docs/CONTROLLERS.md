# Piloter un projet en autonomie

Mode d'emploi des contrôleurs autonomes. Un contrôleur, c'est un cron Hermes qui
se réveille toutes les N minutes, charge le skill `controllers-policy`, et fait
avancer **un** projet : il dispatche des missions, merge des PRs, met à jour son
tracker, et te livre un rapport.

## Créer un contrôleur

Trois choses, dans cet ordre.

**1. Un prompt court.** Uniquement *objectif + gates spécifiques au projet*.
N'y écris pas de règles d'autonomie, de format de rapport, ni de seuils
d'escalade : le skill les porte, et il est rechargé à chaque tick.

```
Contrôleur <nom>. Objectif : <ce qu'il doit faire aboutir>, projet `<slug>`.

Gates du projet :
- <la branche autorisée, ce qu'il ne faut jamais toucher, les invariants>
- Autonomie, format de livraison, trailer `[CTRL: ...]`, escalade et questions
  d'installation sont régis par le skill controllers-policy.
```

**2. Attacher le skill.**

```bash
hermes cron create --name "<nom>" --every 30m \
  --skill controllers-policy --deliver project:<slug> --prompt "$(cat prompt.txt)"
```

`--deliver project:<slug>` est important : la livraison suit le projet, pas une
session qui peut être compactée ou abandonnée. `--deliver origin` gèle la cible
sur la session de création — à éviter pour tout ce qui doit durer. Un job en
`deliver: origin` sans `origin` capturé ne livre nulle part : refuse-le et
recréé-le en `project:<slug>`.

**3. Répondre une fois à ses cinq questions.** Au premier tick, le contrôleur te
demande : périmètre, autorité de merge, plafond de budget, ce qui doit le mettre
en pause, et ce qui mérite une livraison. Tes réponses sont écrites dans son
tracker sous un bloc `GRANT:` — elles survivent ainsi à toute réécriture de
prompt. Il ne les redemandera pas.

## Ce qu'il fait sans demander

Par défaut il **agit** : dispatche, merge une PR verte et dans le périmètre,
tranche une décision défendable et te dit laquelle et pourquoi. Il ne demande
pas la permission.

**Trois choses seulement l'arrêtent** : détruire une donnée irrécupérable,
dépenser hors du budget de sa campagne, ou toucher un dépôt hors périmètre.
Tout le reste lui appartient.

Un prompt « surveillance only / ne jamais relancer » n'est **pas** un
contrôleur. C'est de la passivité déguisée : le skill l'ignore, et le
contrôleur doit soit agir, soit poser **une** question `[DECISION:]`.
Répéter `SCANNER DEAD` n'est pas une escalade.

Corollaire important : « je m'en remets à l'autre contrôleur » est un blocage
déguisé. S'il décline une tâche parce qu'elle « appartient » à quelqu'un
d'autre, il doit vérifier que ce propriétaire est *réellement vivant* dessus
(une mission qui tourne, une PR qui a bougé). Sinon le travail est sans
propriétaire — et le travail sans propriétaire est le sien.

## L'arrêter

Une seule formulation compte :

```
PAUSED(reason=<pourquoi>; resume=<condition vérifiable par une machine>)
```

Toute formulation plus molle (« report only », « ne dispatche pas », « X est le
contrôleur actif ») est traitée comme une dérive de prompt : il agira quand même
et te le signalera pour que tu confirmes ou convertisses en `PAUSED(...)`.

Écris le `resume=` de façon qu'il puisse le vérifier lui-même
(« le périphérique FTDI réapparaît sur spark-de79 ») plutôt que « quand Thomas
confirme ». **Quand la condition est remplie, il lève la pause tout seul** et
reprend — y compris si tu l'as confirmé en conversation. Une pause qui survit à
sa propre condition de reprise est un défaut, et c'est à lui de la corriger.

## Lire son état

Chaque livraison, **y compris les `[SILENT]`**, se termine par :

```
[CTRL: <projet> | mode=active|blocked|paused | wait=<ticks> | next=<prochaine action>]
```

- `mode=active` — il travaille. Un tick sain et muet, c'est `[SILENT]` + ce trailer.
- `mode=blocked` — **aucune lane ne peut avancer**. `wait=` dit depuis combien
  de ticks. Un suffixe `blocked:<cause>` nomme la cause ; c'est le seul suffixe
  autorisé. `blocked` nu n'est pas un fourre-tout.
- `blocked:harness` — CLI manquant, binaire mauvaise arch, `nsenter` cassé.
  Au plus 3 ticks, puis contournement (autre backend, workspace host). Un
  échec de harness n'est **pas** un projet bloqué : rester `mode=active` avec
  `next=` changer de backend / réparer le harness, plutôt que `blocked` nu.
- `mode=paused` — dormant volontairement.

Avant, ces trois régimes te parvenaient tous sous la forme d'un `[SILENT]`
indistinct : impossible de séparer « rien à dire » de « coincé depuis seize
ticks ». Maintenant `grep 'mode=blocked'` suffit, et le board les affiche.

**Rien ne peut rester coincé en silence.** Au bout de 3 ticks sur la même cause,
il doit vérifier que la dépendance est encore vivante, tenter un contournement
borné, et livrer un rapport non silencieux. À 6 ticks, il escalade avec une
question précise.

Un callback d'inspect (`awaiting_user`, mission parkée) ne pose pas
`mode=blocked` : ces statuts omettent le `[CTRL:]`. Recopier l'ancien
trailer est une dérive de prompt.

Si le dispatch est refusé (disque, auth, capacité), le projet **garde son
objectif** avec un blocker infra nommé (`blocked:disk`, …). Le travail
plateforme s'ouvre sous `sandboxed-sh`. On ne retitre pas, on ne réutilise
pas la session de campagne (Lido « Corriger et merger les PRs » devenue un
P0 disque). Un ordre explicite dans le chat (« merge these PRs ») met à jour
le grant (`merge_authority` / `material_bar`) ; un « never merge to main »
périmé ne le surclasse pas.

## Le board

`http://localhost:3001/` — la liste de gauche affiche par projet : le mode, le
nombre de missions vives, un digest de santé (`3 tracks · 1 failing · 2 overdue`)
et un repère `silent` si plus rien n'est arrivé depuis 24 h. Le bandeau du haut
récapitule la flotte (`4 live · 1 blocked · 1 paused`). Le panneau de détail
ajoute le découpage par track, pire d'abord.

Un projet sans trailer `[CTRL:]` s'affiche exactement comme avant — l'absence
n'invente jamais un état.

## Surveiller les budgets

Un cron a un budget `repeat` et **s'auto-désarme quand il est épuisé**, même si
sa campagne tourne encore. À vérifier de temps en temps :

```bash
hermes cron list          # colonne repeat + enabled
hermes cron edit <id> --repeat <n>   # relever le plafond
hermes cron resume <id>              # réarmer
```

## Diagnostiquer

```bash
# état de tous les contrôleurs
hermes cron list

# forcer un tick et voir ce qu'il fait
hermes cron run <id>

# le dernier rapport d'un projet, trailer compris
grep -o '\[CTRL:[^]]*\]' <(hermes chat --resume <session> --last)
```

Si un contrôleur ticque `ok` mais que rien ne bouge, la question à poser est
*quel est son mode*. `active` sans mission créée pendant deux ticks est un défaut
qu'il doit signaler lui-même ; `blocked` avec un `wait=` qui grimpe veut dire que
le contournement a échoué et que l'escalade arrive.

## Coordination between controllers

Plusieurs contrôleurs tournent en parallèle, un (ou plusieurs) par projet, chacun
sur sa propre session de contrôle. Ils **ne se parlent pas directement** (pas de
chat session-à-session). La coordination est **décentralisée**, par trois canaux :

1. **Le substrat git (producteur / consommateur).** Un projet qui *produit* (ex.
   Verity merge ses features de langage dans son repo) et un projet qui *consomme*
   (ex. Lido re-pin sa dépendance sur le HEAD de Verity) se coordonnent en lisant
   l'état git de l'autre. Le consommateur observe le repo du producteur et re-pin
   quand il avance — aucune tâche explicite à s'envoyer.
2. **Lancement de missions cross-projet.** Une session de contrôle peut
   `start_mission` sur un *autre* projet quand elle a besoin de son output : la
   mission porte alors l'`origin_session_id` du demandeur mais le `project` tag de
   la cible. C'est ainsi qu'un contrôleur ajoute une tâche au périmètre d'un autre.
3. **La DB de missions partagée.** Tous les contrôleurs voient toutes les missions
   via les outils MCP (`list_missions`, `get_mission_health`, `list_projects`) —
   visibilité mutuelle, pas de vue privée par contrôleur.

**Évitement de conflit** (il n'y a pas de verrou global unique) :

- **Propriété distincte** : chaque projet a son périmètre ; le pont
  « consommer X » est un projet dédié (ex. `verity-lido`), pas une intrusion dans
  le projet producteur.
- **Verrou de campagne** : une seconde mission de campagne sur le même projet est
  refusée (`409`) tant que la première n'est pas terminée — empêche le doublon.
- **Gates de soundness / revue exact-head** : sur les repos formels, aucun merge
  ne passe sans sa revue exact-head (receipts rejouables, head exact, zéro
  `sorry`/`admit`), ce qui rattrape un travail concurrent divergent au merge.

**Coordinateur central — le fleet-orchestrator (3 composants, pouvoirs
gradués).** État partagé dans `~/.hermes/fleet/state.json`. Trois scripts sous
`~/.hermes/scripts/`, à n'activer que selon la responsabilité qu'on accepte de
leur déléguer :

1. **`fleet-daemon`** *(Couche 1 — observe + ACK + escalate)*. Listener SSE sur
   `/api/control/stream`. ACK le travail terminé, écrit une alerte pour un vrai
   blocker (`awaiting_message`). **Ne supprime, n'annule et ne dispatche
   jamais.** Tourne en permanence via `fleet-daemon.service` (systemd, enabled).
   C'est le socle sûr — vérifier `daemon_last_seen` récent dans `state.json`.
2. **`fleet-watcher`** *(Couche 1 — vision, emit-only)*. Poll toutes les 10 min
   (`fleet-watcher.timer`, `OnCalendar=*:0/10`). Rafraîchit `state.json` et émet
   des signaux urgents — `OVER_SUBSCRIPTION` (plus de missions actives par backend
   que `max_parallel_missions`), `MISSION_STUCK`, `WINDOW_BURNING`,
   `BACKLOG_PRESSURE`. **Aucune action** : les champs `action` qu'il imprime sont
   du texte consultatif. Silencieux quand rien n'est urgent. Sûr à activer.
3. **`fleet-heartbeat`** *(Couche 3 — dispatch autonome)*. **Volontairement non
   planifié.** C'est le seul composant qui *lance* des missions de lui-même ; sa
   responsabilité (dispatch sans humain dans la boucle) est trop large pour être
   armée sans supervision dédiée. Le laisser éteint sauf décision explicite.

Note : depuis que la garde native de sandboxed.sh applique le plafond
`parallel_missions` du grant en dur au `create_mission` (429 `parallel_missions_cap`),
le signal `OVER_SUBSCRIPTION` du watcher est surtout *redondant* pour la
sur-souscription — le watcher n'ajoute plus que la vision quota/backlog. La
coordination reste correcte même watcher éteint (canaux 1–3 + verrous + garde
native).

## Références

- Le contrat complet : `skills/controllers-policy/SKILL.md` sur agent-core.
- Les doctrines d'autonomie, avec les incidents qui les ont produites :
  `references/autonomy-playbook.md`.
- Les questions d'installation et le bloc `GRANT:` :
  `references/controller-setup-questions.md`.
