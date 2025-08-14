# Plan d'implémentation : Magasin de métadonnées externe

Ce document décrit les étapes nécessaires pour implémenter un magasin de métadonnées persistant en utilisant une base de données SQLite. Cette approche remplacera les tentatives précédentes (tags `Comment` et `stickers`) pour associer un ID YouTube aux pistes dans la file d'attente MPD.

## 1. Objectifs

### Objectif principal
Créer un système robuste et flexible pour stocker les métadonnées des pistes YouTube (en commençant par l'ID YouTube) qui ne sont pas nativement gérées par MPD pour les flux.

### Objectifs secondaires
- **Découplage** : Rendre la gestion des métadonnées indépendante des fonctionnalités spécifiques de MPD.
- **Persistance** : Permettre aux métadonnées de persister au-delà de la session actuelle de la file d'attente.
- **Maintenabilité** : Centraliser la logique de gestion des métadonnées dans un seul module dédié.

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
    -   Le module initialisera une base de données SQLite (par ex. `~/.config/rmpc/metadata.db`).
    -   Il créera une table si elle n'existe pas :
      ```sql
      CREATE TABLE IF NOT EXISTS youtube_metadata (
          song_id     INTEGER PRIMARY KEY,
          youtube_id  TEXT NOT NULL
      );
      ```

4.  **Implémenter l'API du `MetadataStore`** :
    -   Créer une `struct MetadataStore` qui contiendra la connexion à la base de données.
    -   Implémenter les méthodes publiques suivantes :
      ```rust
      // Ouvre ou crée la base de données
      pub fn new() -> Result<Self>;

      // Associe un ID de chanson MPD à un ID YouTube
      pub fn add_youtube_song(&self, song_id: u32, youtube_id: &str) -> Result<()>;

      // Récupère l'ID YouTube pour un ID de chanson donné
      pub fn get_youtube_id(&self, song_id: u32) -> Result<Option<String>>;

      // Supprime les métadonnées pour une liste d'IDs de chanson
      pub fn remove_songs(&self, song_ids: &[u32]) -> Result<()>;

      // Vide complètement la table
      pub fn clear(&self) -> Result<()>;
      ```

### Étape 3 : Intégration du `MetadataStore` dans l'application

1.  **Initialisation** :
    -   Instancier le `MetadataStore` au démarrage de l'application (probablement dans `src/core/app.rs`) et le rendre accessible via le contexte global `Ctx`.

2.  **Écriture des métadonnées** :
    -   Dans `src/ui/mod.rs`, dans la fonction `on_youtube_stream_url_ready`, après avoir obtenu le `song_id` de la part de MPD, appeler `ctx.metadata_store.add_youtube_song(song_id, video.id)`.

3.  **Lecture des métadonnées** :
    -   Dans `src/ui/modals/info_list_modal.rs`, modifier l'implémentation de `From<&Song>` :
        -   Elle devra accepter le `MetadataStore` en paramètre.
        -   Appeler `metadata_store.get_youtube_id(song.id)` pour récupérer l'ID YouTube.
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
        4.  Appeler `metadata_store.remove_songs(...)` avec les IDs à supprimer.
    -   Une optimisation sera de gérer les événements de suppression de manière plus ciblée si le protocole le permet facilement.
