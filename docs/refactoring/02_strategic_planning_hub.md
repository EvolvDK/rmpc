# 2. Hub de Planification Stratégique

Ce document centralise la planification stratégique pour l'initiative de refactoring.

### Recommandations de refactoring priorisées

1.  **Décomposer l'Objet Dieu `YouTubePane` (Priorité : Élevée)** : La tâche la plus critique est de décomposer la structure `YouTubePane` dans `src/ui/panes/youtube.rs`. Sa complexité actuelle entrave le développement et augmente le risque de régressions.
    -   **Action :** Extraire la logique et l'état en composants plus petits et ciblés, conformément au Principe de Responsabilité Unique (SRP).
        -   `SearchComponent` : Gérera le champ de saisie de recherche, les modes de recherche, le filtrage et l'affichage des résultats.
        -   `LibraryComponent` : Gérera l'affichage des artistes et des morceaux de la bibliothèque, les sélections et les actions associées.
    -   **Résultat attendu :** Des composants plus petits, plus faciles à comprendre, à maintenir et à tester. `YouTubePane` deviendra un simple conteneur orchestrant ces composants.

2.  **Refactoriser l'implémentation du cache dans `youtube.rs` (Priorité : Moyenne)** : La logique de cache actuelle est dupliquée et utilise des `.unwrap()` risqués.
    -   **Action :** Créer une abstraction de cache centralisée et générique. Supprimer la duplication de code dans `search`, `get_stream_url` et `get_song_info` et utiliser des méthodes de gestion de verrous plus sûres pour éviter les panics.
    -   **Résultat attendu :** Un code plus robuste, plus sûr et plus facile à maintenir. La modification de la stratégie de cache (par exemple, l'ajout de la persistance) sera plus simple à l'avenir.

3.  **Introduire des tests unitaires (Priorité : Élevée)** : Le manque de tests est une dette technique majeure.
    -   **Action :** Écrire des tests unitaires pour la logique métier extraite dans les nouveaux `SearchComponent` et `LibraryComponent`. La testabilité est un objectif principal de la décomposition de `YouTubePane`.
    -   **Résultat attendu :** Une meilleure confiance dans l'exactitude du code, une réduction des régressions et une simplification des futurs refactorings.

### Estimation de l'effort et allocation des ressources

L'effort total est estimé à **5-8 jours-développeur**.

-   **Décomposition de `YouTubePane` : 3-5 jours-développeur**
    -   Jour 1 : Mettre en place la nouvelle structure de fichiers (`youtube/mod.rs`, `youtube/search.rs`, `youtube/library.rs`).
    -   Jours 2-3 : Extraire `SearchComponent` et l'intégrer.
    -   Jours 4-5 : Extraire `LibraryComponent` et l'intégrer.
-   **Refactoring du cache : 0.5-1 jour-développeur**
    -   Cette tâche peut être réalisée en parallèle ou après la décomposition du panneau.
-   **Écriture des tests unitaires : 1-2 jours-développeur**
    -   Les tests doivent être écrits au fur et à mesure que les nouveaux composants sont créés.

**Allocation :** Un développeur dédié devrait être assigné à cette tâche pour assurer la cohérence.

### Évaluation des risques et stratégies d'atténuation

-   **Risque : Régression fonctionnelle dans l'interface utilisateur YouTube (Élevé)**
    -   **Cause :** La complexité élevée de `YouTubePane` rend les changements risqués.
    -   **Stratégie d'atténuation :**
        1.  **Refactoring incrémental :** Décomposer le panneau par petites étapes validées.
        2.  **Tests manuels rigoureux :** Effectuer des tests manuels complets du panneau YouTube après chaque étape significative.
        3.  **Tests unitaires immédiats :** Écrire des tests pour les nouveaux composants dès leur création afin de verrouiller leur comportement.

-   **Risque : Introduction de `panics` dans la logique du cache (Moyen)**
    -   **Cause :** Une gestion incorrecte des verrous (`Mutex`) peut conduire à des deadlocks ou des panics.
    -   **Stratégie d'atténuation :**
        1.  **Revue de code :** Faire examiner spécifiquement la nouvelle logique de cache par un autre développeur.
        2.  **Gestion prudente des erreurs :** Remplacer tous les `.unwrap()` par une gestion appropriée des `Result` pour gérer les verrous "empoisonnés".

-   **Risque : Le refactoring prend plus de temps que prévu (Moyen)**
    -   **Cause :** Une complexité imprévue découverte lors de la décomposition.
    -   **Stratégie d'atténuation :**
        1.  **Communication :** Le développeur doit communiquer régulièrement sur les progrès et les obstacles.
        2.  **Flexibilité du périmètre :** Si nécessaire, se concentrer d'abord sur la décomposition de `YouTubePane` et reporter le refactoring du cache.

### Projections de calendrier et définition des jalons

**Semaine 1 : Décomposition de `YouTubePane`**
-   **Jalon 1 (Fin du Jour 2) :** `SearchComponent` est créé et fonctionnel. La recherche dans l'interface utilisateur est entièrement gérée par le nouveau composant.
-   **Jalon 2 (Fin du Jour 4) :** `LibraryComponent` est créé et fonctionnel. L'affichage de la bibliothèque est géré par le nouveau composant.
-   **Jalon 3 (Fin du Jour 5) :** `YouTubePane` est entièrement refactorisé en un conteneur. Des tests unitaires de base sont en place pour les nouveaux composants.

**Semaine 2 : Finalisation et Améliorations**
-   **Jalon 4 (Fin du Jour 6) :** Le refactoring du module `youtube.rs` et de sa logique de cache est terminé.
-   **Jalon 5 (Fin du Jour 7) :** La couverture des tests unitaires est augmentée. Des tests d'intégration manuels complets ont été effectués et les bogues identifiés ont été corrigés.
-   **Jalon 6 (Fin du Jour 8) :** Le code a été revu, nettoyé et est prêt à être fusionné dans la branche principale.
