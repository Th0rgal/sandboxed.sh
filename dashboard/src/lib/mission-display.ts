const ANIMAL_NAMES = [
  'Ape',
  'Bat',
  'Bear',
  'Bee',
  'Boar',
  'Bison',
  'Bull',
  'Cat',
  'Cod',
  'Crab',
  'Crow',
  'Deer',
  'Dodo',
  'Dove',
  'Duck',
  'Eel',
  'Elk',
  'Emu',
  'Fawn',
  'Finch',
  'Foal',
  'Fox',
  'Frog',
  'Goat',
  'Gull',
  'Hare',
  'Hawk',
  'Heron',
  'Ibis',
  'Jackal',
  'Koala',
  'Lark',
  'Lynx',
  'Manta',
  'Mink',
  'Mole',
  'Moose',
  'Moth',
  'Mule',
  'Newt',
  'Orca',
  'Otter',
  'Owl',
  'Panda',
  'Pika',
  'Pony',
  'Puma',
  'Raven',
  'Seal',
  'Shark',
  'Shrew',
  'Skunk',
  'Snail',
  'Snake',
  'Stag',
  'Swan',
  'Tern',
  'Tiger',
  'Toad',
  'Trout',
  'Viper',
  'Wren',
  'Yak',
  'Zebra',
];

const FNV_OFFSET_BASIS = 2166136261;
const FNV_PRIME = 16777619;

function hashString(input: string): number {
  let hash = FNV_OFFSET_BASIS;
  for (let i = 0; i < input.length; i += 1) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, FNV_PRIME);
  }
  return hash >>> 0;
}

export function getMissionShortName(missionId: string): string {
  const hash = hashString(missionId);
  return ANIMAL_NAMES[hash % ANIMAL_NAMES.length];
}
