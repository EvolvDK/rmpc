# Plan d'implémentation : Système de persistance de données avec SQLite

Ce document décrit les étapes nécessaires pour implémenter un système de persistance de données unifié en utilisant une base de données SQLite. Ce système remplacera les approches précédentes (tags `Comment`, `stickers`) pour les métadonnées de la file d'attente, et migrera également le stockage de la bibliothèque YouTube et des playlists depuis des fichiers JSON.

## 1. Objectifs

### Objectif principal
Créer un système de stockage unique, robuste et performant basé sur SQLite pour toutes les données persistantes de l'application, incluant :
1.  Les métadonnées des pistes YouTube dans la file d'attente MPD (ID YouTube).
2.  La bibliothèque de vidéos YouTube (`youtube_library.json`).
3.  Les playlists YouTube de l'utilisateur (stockées dans le répertoire `yt-playlists/`).

### Objectifs secondaires
- **Consolidation** : Unifier la gestion des données dans un seul module pour améliorer la cohérence et la maintenabilité.
- **Performance et Scalabilité** : Remplacer le parsing JSON par des requêtes SQL pour une meilleure performance, surtout avec de grandes bibliothèques.
- **Intégrité des données** : Utiliser les fonctionnalités de SQLite (transactions, contraintes) pour garantir la robustesse des données.
- **Découplage** : Rendre la gestion des métadonnées indépendante des fonctionnalités spécifiques de MPD.

### Objectif de nettoyage
Éliminer complètement le code des approches précédentes qui se sont avérées inefficaces :
1.  Supprimer toute la logique liée à l'utilisation du tag MPD `Comment` pour stocker l'ID YouTube.
2.  Supprimer toute la logique liée à l'utilisation des `stickers` MPD.

## 2. Étapes d'implémentation

### Étape 1 : Nettoyage des implémentations précédentes

Avant d'écrire du nouveau code, il est crucial de supprimer l'ancien.

1.  **Supprimer la logique des "Stickers"** :
    -   Dans `src/ui/mod.rs` : Annuler l'utilisation de `client.set_sticker(...)` lors de l'ajout d'une chanson.
    -   Dans `src/mpd/mpd_client.rs` : Annuler la modification de `playlist_id` qui récupérait les stickers.
    -   Dans `src/ui/modals/info_list_modal.rs` : Supprimer la logique qui lit `song.stickers`.

2.  **Supprimer la logique du tag "Comment"** :
    -   Dans `src/ui/modals/info_list_modal.rs` : S'assurer que toute logique de lecture du tag `Comment` pour l'ID YouTube est supprimée et que le filtre de tags est propre.

### Étape 2 : Création du module `MetadataStore`

Cette étape consiste à construire le cœur du nouveau système.

1.  **Ajouter la dépendance** :
    -   Ajouter `rusqlite` au `Cargo.toml` pour l'interaction avec la base de données SQLite.

2.  **Créer le nouveau module** :
    -   Créer un nouveau fichier : `src/core/metadata_store.rs`.

3.  **Définir la structure de la base de données** :
    -   Le module initialisera une base de données SQLite unique (par ex. `~/.config/rmpc/rmpc.db`).
    -   Il créera les tables suivantes si elles n'existent pas :
      ```sql
      -- Pour les métadonnées des pistes dans la file d'attente MPD
      CREATE TABLE IF NOT EXISTS queue_youtube_metadata (
          song_id     INTEGER PRIMARY KEY, -- ID de la chanson dans la file d'attente MPD
          youtube_id  TEXT NOT NULL
      );

      -- Pour la bibliothèque de vidéos YouTube (remplace youtube_library.json)
      -- Ne stocke que les métadonnées permanentes.
      CREATE TABLE IF NOT EXISTS videos (
          youtube_id      TEXT PRIMARY KEY NOT NULL,
          title           TEXT NOT NULL,
          channel         TEXT NOT NULL,
          album           TEXT, -- L'album peut être optionnel
          duration_secs   INTEGER NOT NULL,
          thumbnail_url   TEXT -- L'URL de la miniature peut être stockée si elle est stable
      );

      -- Pour définir les playlists (remplace la structure de dossiers yt-playlists/)
      -- Conçue pour contenir à la fois des pistes locales et YouTube.
      CREATE TABLE IF NOT EXISTS playlists (
          id      INTEGER PRIMARY KEY AUTOINCREMENT,
          name    TEXT NOT NULL UNIQUE
      );

      -- Pour lier les pistes (locales ou YouTube) aux playlists
      CREATE TABLE IF NOT EXISTS playlist_items (
          playlist_id         INTEGER NOT NULL,
          position            INTEGER NOT NULL, -- Position dans la playlist
          video_youtube_id    TEXT, -- Pour les pistes YouTube, NULL pour les locales
          file_path           TEXT, -- Pour les pistes locales, NULL pour les YouTube
          PRIMARY KEY (playlist_id, position),
          FOREIGN KEY (playlist_id) REFERENCES playlists(id) ON DELETE CASCADE,
          FOREIGN KEY (video_youtube_id) REFERENCES videos(youtube_id) ON DELETE CASCADE,
          CHECK (video_youtube_id IS NOT NULL OR file_path IS NOT NULL) -- S'assure qu'au moins un type de piste est défini
      );
      ```

4.  **Implémenter l'API du `MetadataStore`** :
    -   Créer une `struct MetadataStore` qui contiendra la connexion à la base de données.
    -   Implémenter une API complète pour gérer à la fois la file d'attente et la bibliothèque/playlists.
      ```rust
      // --- Méthodes générales ---
      pub fn new() -> Result<Self>; // Ouvre ou crée la base de données

      // --- Métadonnées de la file d'attente ---
      pub fn add_youtube_song_to_queue(&self, song_id: u32, youtube_id: &str) -> Result<()>;
      pub fn get_youtube_id_for_song(&self, song_id: u32) -> Result<Option<String>>;
      pub fn remove_songs_from_queue(&self, song_ids: &[u32]) -> Result<()>;
      pub fn clear_queue(&self) -> Result<()>;

      // --- Gestion de la bibliothèque (Vidéos) ---
      pub fn add_video_to_library(&self, video: &YouTubeVideo) -> Result<()>;
      pub fn remove_video_from_library(&self, youtube_id: &str) -> Result<()>;
      pub fn get_all_library_videos(&self) -> Result<Vec<YouTubeVideo>>;
      // ... autres méthodes CRUD pour les vidéos si nécessaire ...

      // --- Gestion des Playlists ---
      pub fn create_playlist(&self, name: &str) -> Result<()>;
      pub fn delete_playlist(&self, playlist_name: &str) -> Result<()>;
      pub fn rename_playlist(&self, old_name: &str, new_name: &str) -> Result<()>;
      pub fn get_all_playlists(&self) -> Result<Vec<Playlist>>; // Playlist contiendrait son nom et ses pistes (locales et YouTube)

      // --- Gestion du contenu des Playlists ---
      pub fn add_youtube_video_to_playlist(&self, playlist_name: &str, youtube_id: &str) -> Result<()>;
      pub fn add_local_file_to_playlist(&self, playlist_name: &str, file_path: &str) -> Result<()>;
      pub fn remove_item_from_playlist(&self, playlist_name: &str, position: usize) -> Result<()>;
      // ... autres méthodes de manipulation de playlist ...
      ```

### Étape 3 : Intégration du `MetadataStore` dans l'application

1.  **Initialisation** :
    -   Instancier le `MetadataStore` au démarrage de l'application (probablement dans `src/core/app.rs`) et le rendre accessible via le contexte global `Ctx`.

2.  **Remplacement de `youtube::storage`** :
    -   Modifier toute la logique qui utilise actuellement `youtube::storage` pour lire/écrire dans les fichiers JSON.
    -   Les panneaux de l'interface utilisateur (comme `youtube_library` et `youtube_playlists`) devront être adaptés pour appeler les nouvelles méthodes du `MetadataStore` via `ctx`.

3.  **Écriture des métadonnées de la file d'attente** :
    -   Dans `src/ui/mod.rs`, dans la fonction `on_youtube_stream_url_ready`, après avoir obtenu le `song_id` de la part de MPD, appeler `ctx.metadata_store.add_youtube_song_to_queue(song_id, video.id)`.

4.  **Lecture des métadonnées de la file d'attente** :
    -   Dans `src/ui/modals/info_list_modal.rs`, modifier l'implémentation de `From<&Song>` pour qu'elle puisse accéder au `MetadataStore` (probablement via `Ctx`).
    -   Appeler `metadata_store.get_youtube_id_for_song(song.id)` pour récupérer l'ID YouTube.
    -   Afficher l'ID s'il est trouvé.

### Étape 4 : Synchronisation avec la file d'attente MPD

C'est l'étape la plus critique pour garantir la cohérence des données.

1.  **Écouter les changements de la file d'attente** :
    -   La boucle `idle` de MPD (probablement dans `src/core/client.rs`) doit écouter le sous-système `queue`.

2.  **Gérer les mises à jour** :
    -   Lorsque l'événement `queue` est reçu, re-synchroniser l'état.
    -   La stratégie la plus simple et la plus robuste est :
        1.  Récupérer la liste complète des `song_id` de la file d'attente MPD.
        2.  Récupérer la liste complète des `song_id` de la table `youtube_metadata`.
        3.  Calculer la différence : les `song_id` qui sont dans la base de données mais plus dans la file d'attente doivent être supprimés.
        4.  Appeler `metadata_store.remove_songs_from_queue(...)` avec les IDs à supprimer.
    -   Une optimisation sera de gérer les événements de suppression de manière plus ciblée si le protocole le permet facilement.

### Étape 5 : Implémentation de la logique anti-doublons

L'objectif est d'empêcher l'ajout de morceaux déjà présents dans la file d'attente, que ce soit des pistes locales ou YouTube.

1.  **Mise à jour du `MetadataStore`** :
    -   Ajouter une nouvelle méthode pour récupérer tous les IDs YouTube actuellement dans la file d'attente :
      ```rust
      // Récupère l'ensemble de tous les IDs YouTube dans la file d'attente
      pub fn get_all_queue_youtube_ids(&self) -> Result<HashSet<String>>;
      ```

2.  **Logique pour les pistes YouTube** :
    -   La logique d'ajout de vidéos YouTube (par exemple, dans `src/ui/panes/youtube.rs` avant d'envoyer la `WorkRequest`) devra être modifiée :
        -   Appeler `ctx.metadata_store.get_all_queue_youtube_ids()` pour obtenir les IDs existants.
        -   Vérifier si l'ID de la vidéo à ajouter est déjà dans cet ensemble.
        -   Si c'est le cas, afficher une notification à l'utilisateur (par exemple via `status_warn!`) et annuler l'ajout.

3.  **Logique pour les pistes locales** :
    -   La logique d'ajout de fichiers locaux (probablement dans les panneaux `Browser` ou `Playlists`) doit être modifiée.
    -   Avant d'envoyer la commande `add` à MPD, il faudra :
        -   Utiliser la file d'attente mise en cache (`ctx.queue`).
        -   Itérer sur les chansons de la file d'attente et vérifier si le chemin du fichier (`song.file`) du morceau à ajouter est déjà présent.
        -   Si c'est le cas, afficher une notification et annuler l'ajout.
