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
- **Action** : Modifier la logique de rendu du panneau de la file d'attente (`QueuePane`). Pour chaque piste YouTube, elle utilisera les caches pour construire et afficher un objet `Song` temporaire enrichi avec les métadonnées correctes (titre, artiste, album, durée).

#### 1.2. Standardisation de la modale "Song Info"
- **Action** : Modifier la modale pour qu'elle affiche un ensemble de champs cohérent pour toutes les pistes.
- **Champs à afficher** :
    - `File`: Chemin local ou URL de streaming.
    - `Filename`: Nom du fichier extrait.
    - `Title`: Titre de la chanson.
    - `Artist`: Artiste ou nom de la chaîne.
    - `Duration`: Durée de la chanson.
    - `Added`: Timestamp d'ajout à la file d'attente (fourni par MPD).
    - `YouTube ID`: Identifiant permanent (uniquement pour les pistes YouTube).

## Goal 2: Gestion robuste de l'expiration des URLs YouTube

### Problème
Les URLs de streaming YouTube expirent, ce qui provoque des échecs de lecture si l'application reste ouverte longtemps.

### Solution

#### 2.1. Gestion de l'expiration au moment de la lecture (Runtime)
- **Principe** : Abandonner la vérification au démarrage au profit d'une gestion "lazy" et plus performante.
- **Action** : Implémenter la logique suivante lorsqu'un utilisateur lance la lecture d'une piste YouTube :
    1.  Tenter de jouer la piste. Si MPD retourne une erreur indiquant que la ressource n'est pas disponible, cela signifie que l'URL a probablement expiré.
    2.  Si la lecture échoue, l'application doit automatiquement et de manière transparente :
        -   Récupérer l'ID YouTube permanent de la piste depuis le `DataStore`.
        -   Demander une nouvelle URL de streaming.
        -   Remplacer l'ancienne chanson dans la file d'attente MPD par une nouvelle avec la nouvelle URL, en utilisant `deleteid` et `addid` pour préserver **exactement la même position**.
        -   Relancer la lecture.

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
