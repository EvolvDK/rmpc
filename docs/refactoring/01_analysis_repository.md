# 1. Référentiel d'Analyse

Ce document sert de référentiel pour toutes les analyses liées à l'effort de refactoring.

### Évaluations détaillées de la qualité du code

#### Analyse de `src/youtube.rs`

Ce module est responsable de toutes les interactions avec l'utilitaire en ligne de commande `yt-dlp`.

**Responsabilités :**
-   Recherche de vidéos sur YouTube.
-   Récupération des URLs de streaming audio.
-   Obtention des métadonnées des chansons.
-   Mise en cache en mémoire des résultats pour améliorer les performances.

**Points forts :**
-   **Séparation des préoccupations :** Il y a une distinction claire entre la structure de données brute issue de `yt-dlp` (`YtDlpRawInfo`) et la structure de données publique et nettoyée (`ResolvedYouTubeSong`).
-   **Encapsulation :** La logique pour résoudre les métadonnées (comme le titre et l'artiste) à partir de plusieurs champs potentiels est bien encapsulée dans des fonctions dédiées (`resolve_title_priority`, `resolve_artist_priority`).
-   **Gestion des processus :** L'utilisation d'un `ChildProcessGuard` est une bonne pratique pour s'assurer que les processus enfants `yt-dlp` sont terminés, évitant ainsi les processus zombies.
-   **Gestion des erreurs :** L'utilisation de `anyhow::Result` simplifie la gestion des erreurs.

**Axes d'amélioration :**
-   **Couplage fort :** Le module est fortement couplé à l'interface en ligne de commande de `yt-dlp`. Tout changement dans le format de sortie de `yt-dlp` pourrait casser le parsing JSON.
-   **Implémentation du cache :** Le mécanisme de cache est basique. La logique de vérification et de mise à jour du cache est dupliquée dans les fonctions `search`, `get_stream_url` et `get_song_info`. L'utilisation de `.lock().unwrap()` sur les Mutex peut entraîner des `panic` si un thread empoisonne le verrou.
-   **Efficacité :** Lancer un nouveau processus pour chaque opération individuelle (`get_stream_url`, `get_song_info`) peut être inefficace en raison de la surcharge liée à la création de processus.

#### Analyse de `src/ui/panes/youtube.rs`

Ce fichier définit un "panneau" complet de l'interface utilisateur pour toutes les fonctionnalités liées à YouTube.

**Responsabilités :**
-   Afficher et gérer le champ de saisie de recherche.
-   Afficher les résultats de recherche et la bibliothèque de l'utilisateur.
-   Gérer la navigation, la sélection et les actions de l'utilisateur (par exemple, ajouter à la file d'attente, supprimer de la bibliothèque).
-   Gérer un état local complexe (focus, sélections, modes de recherche).
-   Rendre l'intégralité du panneau et de ses sous-composants.

**Points forts :**
-   **Gestion de l'état du focus :** L'utilisation d'un `enum Focus` pour gérer quelle partie du panneau est active est une approche claire et efficace pour les TUI.
-   **Découplage :** Le panneau communique avec le reste de l'application via des canaux (`work_sender`, `app_event_sender`), ce qui est une excellente pratique pour les applications asynchrones.
-   **Structure du rendu :** La méthode `render` utilise `ratatui` pour construire la mise en page de manière déclarative, ce qui la rend relativement facile à suivre malgré sa taille.

**Axes d'amélioration :**
-   **Objet Dieu (God Object) :** `YouTubePane` est un exemple classique d'un "God Object". Il a beaucoup trop de responsabilités, ce qui rend le code difficile à lire, à maintenir, à tester et à faire évoluer. C'est le principal point de dette technique.
-   **Complexité élevée :** De nombreuses méthodes, en particulier celles gérant les actions de l'utilisateur (`handle_search_input_action`, `handle_library_songs_action`, etc.), sont très longues et contiennent des logiques conditionnelles profondément imbriquées.
-   **Gestion d'état tentaculaire :** La structure `YouTubePane` contient un très grand nombre de champs pour gérer son état. Cet état pourrait être décomposé en plusieurs sous-structures plus petites et plus ciblées (par exemple, un `SearchState` et un `LibraryState`) pour améliorer la cohésion et la lisibilité.
-   **Longues méthodes :** La méthode `render` et plusieurs gestionnaires d'événements dépassent une longueur raisonnable. Ils pourraient être décomposés en fonctions auxiliaires plus petites pour améliorer la clarté (par exemple, `render_search_column`, `render_library_column`).

### Résultats de l'évaluation de l'architecture

L'interaction entre `youtube.rs` et `youtube.rs` (le panneau UI) est bien définie via le système d'événements de l'application (`AppEvent`, `WorkRequest`). Le module `youtube.rs` agit comme une couche de service pure, tandis que le panneau UI gère toute la logique de présentation.

Cependant, la complexité est presque entièrement concentrée dans le panneau UI. La structure `YouTubePane` est devenue un goulot d'étranglement pour la maintenabilité. L'architecture globale pourrait être grandement améliorée en refactorisant `YouTubePane` en composants plus petits et plus gérables, chacun avec son propre état et sa propre logique.

### Identification des goulots d'étranglement de performance
*(...)*

### Rapports de vulnérabilités de sécurité
*(...)*

### Quantification de la dette technique
*(...)*
