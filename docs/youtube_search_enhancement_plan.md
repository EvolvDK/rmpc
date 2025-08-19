# Amélioration de la recherche YouTube : Modes de recherche avancés

## 1. Contexte

L'objectif est d'améliorer la fonctionnalité de recherche dans la bibliothèque YouTube en introduisant plusieurs modes de recherche, tout en conservant une interface utilisateur épurée. L'utilisateur pourra basculer entre les modes via des raccourcis clavier ou un clic de souris.

### Modes de recherche à implémenter
1.  **Fuzzy** (actuel, par défaut) : Correspondance approximative.
2.  **Contains** : La chaîne de caractères doit être contenue dans le résultat.
3.  **Starts With** : Le résultat doit commencer par la chaîne de caractères.
4.  **Exact Match** : Correspondance exacte.
5.  **Regex** : Utilisation d'expressions régulières.

---

## 2. Plan d'implémentation par objectifs

### Objectif 1 : Implémenter la logique des modes de recherche

Le socle de la fonctionnalité. Il s'agit de mettre en place la mécanique de recherche sans toucher à l'interface.

-   **Action 1.1 : Créer l'énumération `SearchMode`**
    -   Définir une énumération `SearchMode` dans un module approprié (par exemple, `src/ui/panes/youtube.rs`) avec les variantes : `Fuzzy`, `Contains`, `StartsWith`, `Exact`, `Regex`.
    -   Implémenter une méthode `next()` et `previous()` pour permettre de passer cycliquement d'un mode à l'autre.

-   **Action 1.2 : Gérer l'état du mode de recherche**
    -   Ajouter un champ `youtube_search_mode: SearchMode` à la structure de l'état de l'application (probablement `Ctx` ou l'état du panneau YouTube).
    -   Initialiser ce champ avec `SearchMode::Fuzzy` par défaut.

-   **Action 1.3 : Implémenter la logique de filtrage**
    -   Modifier la fonction qui filtre les résultats de la recherche YouTube.
    -   Cette fonction devra lire le mode de recherche actuel depuis l'état et appliquer la logique de filtrage correspondante (approximative, contient, commence par, etc.) sur les résultats.

### Objectif 2 : Mettre à jour l'interface utilisateur

Rendre le mode de recherche actuel visible et interactif pour l'utilisateur.

-   **Action 2.1 : Afficher le mode dans le champ de recherche**
    -   Modifier le widget de saisie de la recherche (`Input`) pour qu'il affiche le nom du mode de recherche actuel entre crochets, avant le texte de la requête.
    -   Exemple : `[Fuzzy] Votre recherche...`

-   **Action 2.2 : Gérer le clic sur l'indicateur de mode**
    -   Rendre la zone de l'indicateur de mode (ex: `[Fuzzy]`) cliquable.
    -   Un clic sur cette zone devra déclencher le passage au mode de recherche suivant, en mettant à jour l'état.

### Objectif 3 : Ajouter les raccourcis clavier

Offrir un contrôle efficace pour les utilisateurs avancés.

-   **Action 3.1 : Gérer le cycle des modes**
    -   Dans le gestionnaire d'événements du panneau de recherche YouTube, intercepter le raccourci `Ctrl + M` pour passer au mode de recherche suivant.
    -   Intercepter `Ctrl + Shift + M` pour passer au mode de recherche précédent.
    -   Ces actions mettront à jour l'état et déclencheront un nouveau rendu du champ de recherche.

### Objectif 4 : Mettre à jour la documentation utilisateur

Assurer la découvrabilité des nouvelles fonctionnalités.

-   **Action 4.1 : Ajouter les raccourcis à la fenêtre d'aide**
    -   Identifier le code qui génère le contenu de la modale d'aide (`ShowHelp`, accessible via `~`).
    -   Ajouter les nouvelles combinaisons de touches (`Ctrl + M`, `Ctrl + Shift + M`) et leur description à la liste des raccourcis disponibles pour le panneau de recherche YouTube.
