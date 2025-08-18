# Plan de refactorisation : Amélioration de la gestion des métadonnées et de la lecture

Ce document formalise le plan d'action pour résoudre les problèmes d'affichage des métadonnées dans la file d'attente et pour gérer de manière robuste l'expiration des URLs de streaming YouTube.

## Goal 1: Affichage unifié et enrichi des métadonnées

### Problème
L'affichage actuel de la file d'attente est incohérent. Les pistes YouTube et locales affichent "Unknown" pour le titre et l'artiste, et la modale "Song Info" présente des informations différentes selon le type de piste.

### Solution

#### 1.1. Enrichissement de la file d'attente
- **Action** : Implémenter un système de cache dans le contexte (`Ctx`) pour les métadonnées YouTube.
    - Ajouter un cache `youtube_library: HashMap<String, YouTubeVideo>` pour un accès rapide aux métadonnées par ID YouTube.
    - Ajouter un cache `queue_youtube_ids: HashMap<u32, String>` pour lier les ID de chanson MPD aux ID YouTube.
- **Action** : Initialiser ces caches au démarrage de l'application en lisant les données depuis le `DataStore`.
- **Action** : Modifier la logique de rendu de tous les composants de l'interface (`QueuePane`, en-tête, etc.) qui affichent des informations sur les chansons. Pour chaque piste YouTube, ils utiliseront les caches pour construire et afficher un objet `Song` temporaire enrichi avec les métadonnées correctes (titre, artiste, album, durée), garantissant une cohérence visuelle dans toute l'application.

#### 1.2. Standardisation de la modale "Song Info"
- **Action** : Modifier la modale pour qu'elle affiche un ensemble de champs cohérent pour toutes les pistes.
- **Champs à afficher** :
    - `File`: Chemin local ou URL de streaming.
    - `Filename`: Nom du fichier extrait.
    - `Title`: Titre de la chanson.
    - `Artist`: Artiste ou nom de la chaîne.
    - `Duration`: Durée de la chanson.
    - `Added`: Timestamp d'ajout à la file d'attente (fourni par MPD).
    - `Updated`: Timestamp de mise à jour de la chanson dans la file d'attente (nouvelle URL de streaming, nouveau métadata).
    - `YouTube ID`: Identifiant permanent (uniquement pour les pistes YouTube).

#### 1.3. Implémentation du timestamp "Updated"
- **Problème**: La modale doit afficher un timestamp "Updated" pour savoir quand les métadonnées ou l'URL de streaming ont été rafraîchies. Cette information n'est pas stockée actuellement.
- **Solution**:
    - **Modification de la base de données**: Ajouter une colonne `updated_at` à la table `queue_youtube_metadata` pour stocker ce timestamp.
      ```sql
      CREATE TABLE IF NOT EXISTS queue_youtube_metadata (
          song_id     INTEGER PRIMARY KEY,
          youtube_id  TEXT NOT NULL,
          updated_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
      );
      ```
    - **Mise à jour de l'API `DataStore`**:
        -   Modifier la méthode `get_youtube_id_for_song` pour qu'elle retourne également le timestamp `updated_at`.
        -   Créer une nouvelle méthode `touch_youtube_song(song_id: u32)` qui met à jour le champ `updated_at` à l'heure actuelle pour une chanson donnée.
    - **Intégration**:
        -   Lorsqu'une chanson YouTube est ajoutée à la file d'attente, le `DEFAULT CURRENT_TIMESTAMP` initialisera le champ `updated_at`.
        -   Lors du rafraîchissement réussi d'une URL de streaming (décrit dans le Goal 2), l'application appellera `data_store.touch_youtube_song()` pour mettre à jour le timestamp.
        -   La modale "Song Info" lira cette nouvelle information depuis le `DataStore` pour l'afficher.

## Goal 2: Gestion robuste de l'expiration des URLs YouTube

### Problème
Les URLs de streaming YouTube expirent, ce qui provoque des échecs de lecture si l'application reste ouverte longtemps.

### Solution

#### 2.1. Gestion de l'expiration au moment de la lecture (Runtime)
- **Principe** : Abandonner la vérification au démarrage au profit d'une gestion "lazy" et plus performante.
- **Action** : Implémenter la logique suivante lorsqu'un utilisateur lance la lecture d'une piste YouTube :
    1.  Tenter de jouer la piste. Si MPD retourne une erreur indiquant que la ressource n'est pas disponible (par exemple, erreur `NoExist`), l'application doit déclencher un processus de rafraîchissement.
    2.  Ce processus doit être **asynchrone** (via une `WorkRequest`) pour ne pas bloquer l'interface utilisateur.
    3.  **Si le rafraîchissement réussit** :
        -   Récupérer l'ID YouTube permanent de la piste depuis le `DataStore`.
        -   Demander une nouvelle URL de streaming **et les métadonnées à jour** de la vidéo (via `yt-dlp`).
        -   Comparer les métadonnées obtenues avec celles stockées dans la table `videos` et les mettre à jour si elles ont changé.
        -   Remplacer l'ancienne chanson dans la file d'attente MPD par une nouvelle avec la nouvelle URL, en utilisant `deleteid` et `addid` pour préserver **exactement la même position**.
        -   Relancer la lecture.
    4.  **Si le rafraîchissement échoue** (par exemple, vidéo supprimée, problème réseau) :
        -   Informer l'utilisateur via la barre de statut que la piste est indisponible.
        -   Éviter de retenter la lecture en boucle. Idéalement, sauter à la piste suivante si la lecture automatique est activée.

## Goal 3: Amélioration du feedback utilisateur et des logs

### Problème
L'utilisateur n'est pas informé des actions automatiques de l'application, comme le rafraîchissement des URLs.

### Solution

#### 3.1. Messages de log clairs
- **Action** : Ajouter des logs détaillés pour tracer le cycle de vie du rafraîchissement des URLs :
    -   Lorsqu'une URL expirée est détectée (échec de lecture).
    -   Lorsqu'une nouvelle URL est demandée et obtenue.
    -   Lors du remplacement de la chanson dans la file d'attente MPD.

#### 3.2. Mises à jour de la barre de statut
- **Action** : Fournir un retour visuel discret à l'utilisateur.
    -   Afficher un message dans la barre de statut lorsque le rafraîchissement est en cours (par exemple : "Refreshing stream for 'Titre de la chanson'...").

## Goal 4: Persistance et synchronisation de la file d'attente au redémarrage

### Problème
Après un redémarrage du système, les métadonnées des pistes YouTube dans la file d'attente sont perdues car la base de données `rmpc.db` n'est pas synchronisée avec l'état de la file d'attente de MPD. Cela empêche l'affichage correct des métadonnées et le fonctionnement du rafraîchissement des URLs.

### Solution
- **Principe**: L'approche précédente utilisant les tags `Comment` de MPD s'est avérée non fiable. La nouvelle stratégie consiste à intégrer l'ID YouTube directement dans l'URL de streaming, car le champ `file` de la chanson est garanti de persister.
- **Action 1: Modification de l'URL à l'ajout**:
    -   Lorsqu'une chanson YouTube est ajoutée à la file d'attente, après avoir obtenu l'URL de streaming, y ajouter un paramètre de requête personnalisé.
    -   Le paramètre aura un format standardisé : `&rmpc_yt_id=VIDEO_ID`.
    -   C'est cette URL modifiée qui sera ajoutée à la file d'attente de MPD.
- **Action 2: Synchronisation au démarrage**:
    -   Au lancement de l'application, implémenter une routine de synchronisation qui lit l'intégralité de la file d'attente de MPD.
    -   Pour chaque chanson, analyser son champ `file` (l'URL) pour y chercher le paramètre `rmpc_yt_id=...`.
    -   Si le paramètre est trouvé, extraire l'ID de la chanson MPD et l'ID YouTube pour peupler la table `queue_youtube_metadata` dans `rmpc.db`.
- **Action 3: Nettoyage de l'ancienne implémentation**:
    -   Supprimer toute la logique de code liée à l'ajout de tags via `addtagid` pour le `Comment`.
    -   Retirer toutes les fonctions qui tentent de lire ou d'analyser les tags `Comment` pour la synchronisation. Le concept de tag `Comment` pour la persistance doit être complètement éliminé du programme.
- **Action 4: Mise à jour de l'API `DataStore`**:
    -   Créer une nouvelle méthode `sync_queue_from_mpd(&self, songs: &[Song])` qui effectuera cette nouvelle synchronisation basée sur l'URL en une seule transaction pour des raisons de performance.

## Goal 5: Amélioration de la lisibilité des textes longs

### Problème
Les textes longs (titres de chansons, noms de chaînes) sont tronqués dans plusieurs composants de l'interface utilisateur (panneau d'aperçu, listes de la bibliothèque et des résultats de recherche), ce qui rend difficile la lecture des informations complètes.

### Solution

#### 5.1. Affichage multi-lignes dans l'aperçu (Preview)
- **Action**: Modifier la logique de rendu du panneau d'aperçu (`render_preview_data`) pour que les valeurs longues des métadonnées puissent s'afficher sur plusieurs lignes (`wrapping`) au lieu d'être tronquées.

#### 5.2. Affichage temporaire dans la barre de statut pour les listes
- **Action**: Implémenter un mécanisme d'affichage statique et temporaire pour les éléments sélectionnés dans les listes.
    -   Lorsqu'un utilisateur navigue dans une liste (résultats de recherche, bibliothèque) et qu'un nouvel élément est sélectionné, son texte complet sera affiché dans la barre de statut via `status_info!`.
    -   Pour éviter d'encombrer l'interface, ce message disparaîtra automatiquement après un court délai (par exemple, 3 secondes).
    -   **Implémentation technique**: Utiliser le planificateur d'événements (`scheduler`) pour envoyer un événement de "nettoyage de la barre de statut" après le délai imparti.
